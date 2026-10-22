//! Native parsing of the pacman local database, and the compact path index.
//!
//! A pacman package database lives under `<db-path>/local/<pkg>-<ver>/`:
//!
//! * `files` — a `%FILES%` header followed by one path per line. Paths are
//!   package-root-relative (normalized here to absolute `/...` paths) and are
//!   **verbatim**: spaces are preserved. Directories keep a trailing `/`.
//! * `desc` — `%NAME%`, `%VERSION%` and `%BACKUP%` (one path per line). Backup
//!   paths are ordinary owned paths here; a modified backup file is `dirty`
//!   like any other, because it is not reproducible from the package.
//! * `mtree` — a gzipped BSD mtree. `/set` lines provide defaults (`type`,
//!   `uid`, `gid`, `mode`, ...), entries are `./path key=value ...` lines, and
//!   spaces/backslashes in paths are octal escapes (`\040`, `\134`, ...).
//!
//! # Why the index is shaped the way it is
//!
//! The index for a full Arch installation owns ~480 000 paths. The obvious
//! `BTreeMap<String, OwnedPath>` representation costs ~450 MiB once every path,
//! package name and mtree digest is a heap string, and serializing it as JSON
//! needed another ~123 MiB buffer — more than the host's per-detector memory
//! cap. Instead the index is built as three compact cache entries:
//!
//! * the **meta** entry holds the interned package table, the shared string
//!   arena (symlink targets and diagnostics) and a Bloom filter over the path
//!   hashes. It is small and loaded in full (`cache_get`).
//! * the **table** entry is an open-addressed `hash → record` table. It is
//!   loaded in full but is only ~12 bytes per slot.
//! * the **records** entry holds one fixed-size metadata record per owned path
//!   (plus an optional checksum arena). It is read **one record at a time**
//!   through `cache-read`, so a scan never materializes the whole index.
//!
//! Lookups hash the path, reject negatives with the Bloom filter, probe the
//! in-memory table and only then read the matching record. Because only the
//! hash is stored, a path is never kept as a string in guest memory; a 64-bit
//! hash collision (negligible) is the only way two paths can be confused.
//!
//! The index is restricted to paths at or below the crawl root, so a scan of
//! `~` or a project tree only touches the packages that can actually be asked
//! about. A corrupt cache entry is ignored and rebuilt; an unreadable database
//! is an `init` error (the host disables the plugin with a visible note).
//! Unparseable *entries* never become a wrong `clean`: they are flagged and the
//! detector reports `unknown`.
//!
//! The database format is stable but not a public API. This parser knows the
//! fields the detector needs and ignores everything else.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use backupcrawl_plugin_sdk::{Host, LogLevel};
use flate2::read::GzDecoder;

/// Version of the serialized cache format. Bump when the index layout changes.
pub const INDEX_FORMAT: u32 = 3;

/// One fixed-size metadata record per owned path.
const RECORD_LEN: usize = 52;
/// One open-addressed table slot: `hash: u64`, `record index: u32`.
const SLOT_LEN: usize = 12;
/// One checksum entry: md5 (16 bytes) followed by sha256 (32 bytes).
const DIGEST_LEN: usize = 48;
/// Sentinel for "absent" in a record or an empty table slot.
const NONE_U32: u32 = u32::MAX;
const NONE_U64: u64 = u64::MAX;
const NONE_I64: i64 = i64::MIN;
/// Magic of the meta cache entry.
const META_MAGIC: &[u8; 8] = b"BCRPMXD\x00";
/// Target table load factor (`capacity = estimate * 100 / LOAD_PERCENT`).
const TABLE_LOAD_PERCENT: usize = 80;
/// Bloom filter sizing (≈1 % false-positive rate).
const BLOOM_BITS_PER_ENTRY: u64 = 10;
const BLOOM_HASHES: u32 = 7;

/// The package that owns a path and its expected mtree entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedPath {
    /// Package name (`desc` `%NAME%`).
    pub package: String,
    /// Package version (`desc` `%VERSION%`).
    pub version: String,
    /// Whether the path is listed in `%BACKUP%`.
    pub backup: bool,
    /// Expected mtree entry, or `None` when the package has no usable entry
    /// for this path (missing/unreadable mtree, or a `files` line without a
    /// matching mtree line).
    pub mtree: Option<MtreeEntry>,
}

/// Entry type from the mtree `type` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MtreeKind {
    /// Regular file.
    File,
    /// Directory.
    Dir,
    /// Symbolic link.
    Link,
}

/// One parsed mtree entry. Optional fields are only compared when present.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MtreeEntry {
    /// Expected object type.
    pub kind: Option<MtreeKind>,
    /// Expected size in bytes.
    pub size: Option<u64>,
    /// Expected modification time, seconds since the Unix epoch.
    pub time: Option<i64>,
    /// Expected permission bits (`0o7777`).
    pub mode: Option<u32>,
    /// Expected owner uid.
    pub uid: Option<u32>,
    /// Expected owner gid.
    pub gid: Option<u32>,
    /// Expected symlink target, verbatim.
    pub link: Option<String>,
    /// Expected MD5 digest (lowercase hex).
    pub md5: Option<String>,
    /// Expected SHA-256 digest (lowercase hex).
    pub sha256: Option<String>,
    /// Why this entry cannot be trusted; a claim on it is `unknown`.
    pub invalid: Option<String>,
}

impl MtreeEntry {
    fn invalidate(&mut self, message: impl Into<String>) {
        match &mut self.invalid {
            Some(existing) => {
                existing.push_str("; ");
                existing.push_str(&message.into());
            }
            None => self.invalid = Some(message.into()),
        }
    }
}

/// An interned package name + version.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Package {
    name: String,
    version: String,
}

/// A Bloom filter over path hashes, used to reject non-owned paths without a
/// table probe or a record read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Bloom {
    bits: Vec<u8>,
    bit_count: u64,
    hash_count: u32,
}

impl Bloom {
    /// Sizes a filter for `entries` keys at roughly a 1 % false-positive rate.
    fn for_entries(entries: usize) -> Self {
        let bit_count = ((entries as u64) * BLOOM_BITS_PER_ENTRY).max(64);
        let bytes = bit_count.div_ceil(8) as usize;
        Self {
            bits: vec![0; bytes],
            bit_count,
            hash_count: BLOOM_HASHES,
        }
    }

    fn insert(&mut self, hash: u64) {
        let second = hash.rotate_left(32) | 1;
        for index in 0..self.hash_count as u64 {
            let bit = hash.wrapping_add(index.wrapping_mul(second)) % self.bit_count;
            self.bits[(bit / 8) as usize] |= 1 << (bit % 8);
        }
    }

    fn contains(&self, hash: u64) -> bool {
        let second = hash.rotate_left(32) | 1;
        for index in 0..self.hash_count as u64 {
            let bit = hash.wrapping_add(index.wrapping_mul(second)) % self.bit_count;
            if self.bits[(bit / 8) as usize] & (1 << (bit % 8)) == 0 {
                return false;
            }
        }
        true
    }
}

/// Where the per-path metadata records live.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Records {
    /// Held in guest memory (a freshly built index, or the fallback when the
    /// cache is unavailable).
    Memory(Vec<u8>),
    /// Read on demand from a cache entry.
    Cache(String),
}

/// The deterministic path → package/mtree index for one database snapshot.
///
/// Only paths at or below the scan's crawl root are present; the detector is
/// never asked about anything else. See the module docs for the layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    /// Cache format version.
    format: u32,
    /// Interned package name/version table, indexed by the record's `package`.
    packages: Vec<Package>,
    /// Shared string arena (`link` and `invalid` fields).
    strings: Vec<u8>,
    /// Fast negative filter over path hashes.
    bloom: Bloom,
    /// Open-addressed `hash → record index` table (`capacity * SLOT_LEN`).
    table: Vec<u8>,
    /// Number of owned paths (records).
    count: u32,
    /// Whether the index carries a checksum arena.
    digests: bool,
    /// Number of checksum entries.
    digest_count: u32,
    /// Where the records live.
    records: Records,
}

impl Index {
    /// An empty index in the current format.
    pub fn empty() -> Self {
        Self {
            format: INDEX_FORMAT,
            packages: Vec::new(),
            strings: Vec::new(),
            bloom: Bloom::for_entries(0),
            table: Vec::new(),
            count: 0,
            digests: false,
            digest_count: 0,
            records: Records::Memory(Vec::new()),
        }
    }

    /// Number of owned paths.
    pub fn len(&self) -> usize {
        self.count as usize
    }

    /// Whether the index owns no paths.
    #[allow(dead_code)] // paired with `len` for the usual container API
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Builds an in-memory index from an explicit path map.
    ///
    /// The unit tests use this so they exercise the same hash/table/record
    /// lookup path as a real cache-backed index.
    #[cfg(test)]
    pub fn from_entries(entries: &BTreeMap<String, OwnedPath>) -> Self {
        let mut builder = Builder::new(entries.len(), true);
        for (path, owned) in entries {
            builder.insert(path, owned);
        }
        builder.finish()
    }

    /// Looks up one absolute, lexically normalized database key.
    ///
    /// Returns `None` when the path is not owned. A lazy record read that fails
    /// falls back to loading the whole records entry once; if that also fails
    /// the path is reported unowned (never a wrong `clean`).
    pub fn lookup(&mut self, key: &str, host: &mut dyn Host) -> Option<OwnedPath> {
        let hash = path_hash(key);
        if !self.bloom.contains(hash) {
            return None;
        }
        let slots = self.table.len() / SLOT_LEN;
        if slots == 0 {
            return None;
        }
        let mut pos = (hash % slots as u64) as usize;
        let mut index = NONE_U32;
        for _ in 0..slots {
            let offset = pos * SLOT_LEN;
            if read_u32(&self.table, offset + 8) == NONE_U32 {
                return None;
            }
            if read_u64(&self.table, offset) == hash {
                index = read_u32(&self.table, offset + 8);
                break;
            }
            pos = (pos + 1) % slots;
        }
        if index == NONE_U32 {
            return None;
        }

        let record = self.record(index as usize, host)?;
        let digest_index = read_u32(&record, 44);
        let digest = if self.digests && digest_index != NONE_U32 {
            self.digest(digest_index as usize, host)
        } else {
            None
        };
        Some(self.decode(&record, digest.as_deref()))
    }

    /// Reads (and, on the first failure, reloads) one metadata record.
    fn record(&mut self, index: usize, host: &mut dyn Host) -> Option<Vec<u8>> {
        let key = match &self.records {
            Records::Memory(bytes) => {
                let offset = index * RECORD_LEN;
                return (offset + RECORD_LEN <= bytes.len())
                    .then(|| bytes[offset..offset + RECORD_LEN].to_vec());
            }
            Records::Cache(key) => key.clone(),
        };

        let offset = (index * RECORD_LEN) as u64;
        match host.cache_read(&key, offset, RECORD_LEN as u32) {
            Ok(Some(bytes)) if bytes.len() == RECORD_LEN => return Some(bytes),
            Ok(_) => {}
            Err(err) => host.log(
                LogLevel::Warn,
                &format!("pacman: cannot read index record: {}", err.message()),
            ),
        }
        self.reload_records(&key, host);
        match &self.records {
            Records::Memory(bytes) => {
                let offset = index * RECORD_LEN;
                (offset + RECORD_LEN <= bytes.len())
                    .then(|| bytes[offset..offset + RECORD_LEN].to_vec())
            }
            Records::Cache(_) => None,
        }
    }

    /// Reads one checksum entry (`md5` then `sha256`) from the records entry.
    fn digest(&mut self, index: usize, host: &mut dyn Host) -> Option<Vec<u8>> {
        let records_len = self.count as usize * RECORD_LEN;
        let key = match &self.records {
            Records::Memory(bytes) => {
                let offset = records_len + index * DIGEST_LEN;
                return (offset + DIGEST_LEN <= bytes.len())
                    .then(|| bytes[offset..offset + DIGEST_LEN].to_vec());
            }
            Records::Cache(key) => key.clone(),
        };

        let offset = (records_len + index * DIGEST_LEN) as u64;
        match host.cache_read(&key, offset, DIGEST_LEN as u32) {
            Ok(Some(bytes)) if bytes.len() == DIGEST_LEN => return Some(bytes),
            Ok(_) => {}
            Err(err) => host.log(
                LogLevel::Warn,
                &format!("pacman: cannot read index digest: {}", err.message()),
            ),
        }
        self.reload_records(&key, host);
        match &self.records {
            Records::Memory(bytes) => {
                let offset = records_len + index * DIGEST_LEN;
                (offset + DIGEST_LEN <= bytes.len())
                    .then(|| bytes[offset..offset + DIGEST_LEN].to_vec())
            }
            Records::Cache(_) => None,
        }
    }

    /// Loads the whole records entry after a ranged read failed, so the rest of
    /// the scan keeps working even if `cache-read` misbehaves mid-scan.
    fn reload_records(&mut self, key: &str, host: &mut dyn Host) {
        if matches!(self.records, Records::Memory(_)) {
            return;
        }
        match host.cache_get(key) {
            Some(bytes) if bytes.len() >= self.count as usize * RECORD_LEN => {
                host.log(
                    LogLevel::Debug,
                    "pacman: reloaded the whole index after a ranged read failed",
                );
                self.records = Records::Memory(bytes);
            }
            _ => host.log(
                LogLevel::Warn,
                "pacman: index records are unreadable; treating paths as unowned",
            ),
        }
    }

    /// Decodes one record into the detector's view.
    fn decode(&self, record: &[u8], digest: Option<&[u8]>) -> OwnedPath {
        let package = read_u32(record, 28);
        let package = self.packages.get(package as usize);
        let flags = record[48];
        let kind = match flags & 0b11 {
            1 => Some(MtreeKind::File),
            2 => Some(MtreeKind::Dir),
            3 => Some(MtreeKind::Link),
            _ => None,
        };
        let mtree = (flags & (1 << 3) != 0).then(|| MtreeEntry {
            kind,
            size: opt_u64(read_u64(record, 0)),
            time: opt_i64(read_i64(record, 8)),
            mode: opt_u32(read_u32(record, 16)),
            uid: opt_u32(read_u32(record, 20)),
            gid: opt_u32(read_u32(record, 24)),
            link: self.string(read_u32(record, 32), u32::from(read_u16(record, 36))),
            invalid: self.string(read_u32(record, 38), u32::from(read_u16(record, 42))),
            md5: (flags & (1 << 4) != 0)
                .then(|| digest.and_then(|bytes| bytes.get(..16)).map(hex::encode))
                .flatten(),
            sha256: (flags & (1 << 5) != 0)
                .then(|| digest.and_then(|bytes| bytes.get(16..48)).map(hex::encode))
                .flatten(),
        });
        OwnedPath {
            package: package.map(|p| p.name.clone()).unwrap_or_default(),
            version: package.map(|p| p.version.clone()).unwrap_or_default(),
            backup: flags & (1 << 2) != 0,
            mtree,
        }
    }

    /// Decodes a `(offset, length)` slice of the shared string arena.
    fn string(&self, offset: u32, length: u32) -> Option<String> {
        if offset == NONE_U32 {
            return None;
        }
        let start = offset as usize;
        let end = start.checked_add(length as usize)?;
        if end > self.strings.len() {
            return None;
        }
        Some(String::from_utf8_lossy(&self.strings[start..end]).into_owned())
    }

    /// Serializes the small meta entry (packages, strings, Bloom filter).
    fn encode_meta(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.strings.len() + self.packages.len() * 32);
        out.extend_from_slice(META_MAGIC);
        put_u32(&mut out, self.format);
        put_u32(&mut out, u32::from(self.digests as u8));
        put_u32(&mut out, self.count);
        put_u32(&mut out, (self.table.len() / SLOT_LEN) as u32);
        put_u32(&mut out, self.digest_count);
        put_u64(&mut out, self.bloom.bit_count);
        put_u32(&mut out, self.bloom.hash_count);
        put_u32(&mut out, self.bloom.bits.len() as u32);
        out.extend_from_slice(&self.bloom.bits);
        put_u32(&mut out, self.strings.len() as u32);
        out.extend_from_slice(&self.strings);
        put_u32(&mut out, self.packages.len() as u32);
        for package in &self.packages {
            put_u16(&mut out, package.name.len() as u16);
            put_u16(&mut out, package.version.len() as u16);
            out.extend_from_slice(package.name.as_bytes());
            out.extend_from_slice(package.version.as_bytes());
        }
        out
    }

    /// Parses a meta entry and pairs it with a freshly loaded table.
    fn from_meta(meta: &[u8], table: Vec<u8>, records_key: String) -> Result<Self, String> {
        let mut reader = Bytes::new(meta);
        if reader.take(8)? != META_MAGIC {
            return Err("bad meta magic".to_owned());
        }
        let format = reader.u32()?;
        if format != INDEX_FORMAT {
            return Err(format!("unknown index format {format}"));
        }
        let flags = reader.u32()?;
        let count = reader.u32()?;
        let slots = reader.u32()?;
        let digest_count = reader.u32()?;
        let bit_count = reader.u64()?;
        let hash_count = reader.u32()?;
        let bloom_len = reader.u32()? as usize;
        let bloom_bits = reader.take(bloom_len)?.to_vec();
        let strings_len = reader.u32()? as usize;
        let strings = reader.take(strings_len)?.to_vec();
        let packages_len = reader.u32()? as usize;
        let mut packages = Vec::with_capacity(packages_len);
        for _ in 0..packages_len {
            let name_len = reader.u16()? as usize;
            let version_len = reader.u16()? as usize;
            let name = std::str::from_utf8(reader.take(name_len)?)
                .map_err(|err| format!("package name: {err}"))?
                .to_owned();
            let version = std::str::from_utf8(reader.take(version_len)?)
                .map_err(|err| format!("package version: {err}"))?
                .to_owned();
            packages.push(Package { name, version });
        }
        if table.len() != slots as usize * SLOT_LEN {
            return Err(format!(
                "table is {} bytes, expected {}",
                table.len(),
                slots as usize * SLOT_LEN
            ));
        }
        Ok(Self {
            format,
            packages,
            strings,
            bloom: Bloom {
                bits: bloom_bits,
                bit_count,
                hash_count,
            },
            table,
            count,
            digests: flags & 1 != 0,
            digest_count,
            records: Records::Cache(records_key),
        })
    }
}

impl Default for Index {
    fn default() -> Self {
        Self::empty()
    }
}

/// Validates the fixed header of a meta entry before the table is fetched, so
/// a corrupt entry is reported as corrupt rather than as a missing table.
fn validate_meta(meta: &[u8]) -> Result<(), String> {
    if meta.len() < META_MAGIC.len() {
        return Err("truncated meta entry".to_owned());
    }
    if &meta[..META_MAGIC.len()] != META_MAGIC {
        return Err("bad meta magic".to_owned());
    }
    if meta.len() < 12 {
        return Err("truncated meta entry".to_owned());
    }
    let format = read_u32(meta, 8);
    if format != INDEX_FORMAT {
        return Err(format!("unknown index format {format}"));
    }
    Ok(())
}

/// Builds (or loads from the host cache) the index for `<db_path>/local`,
/// restricted to `root` and its descendants.
pub fn load_index(
    db_path: &Path,
    root: &str,
    level: &str,
    host: &mut dyn Host,
) -> Result<Index, String> {
    let local = db_path.join("local");
    let metadata = std::fs::metadata(&local).map_err(|err| {
        format!(
            "pacman database not found at {} ({err}); set [plugins.pacman] db-path",
            local.display()
        )
    })?;
    if !metadata.is_dir() {
        return Err(format!(
            "pacman database path {} is not a directory",
            local.display()
        ));
    }

    let base = cache_key(db_path, root, level, metadata.modified().ok());
    let table_key = format!("{base}:t");
    let records_key = format!("{base}:r");

    if let Some(meta) = host.cache_get(&base) {
        match validate_meta(&meta) {
            Ok(()) => match host.cache_get(&table_key) {
                Some(table) => match Index::from_meta(&meta, table, records_key.clone()) {
                    Ok(index) => {
                        host.log(
                            LogLevel::Debug,
                            &format!("pacman: index cache hit ({} paths)", index.len()),
                        );
                        return Ok(index);
                    }
                    Err(err) => host.log(
                        LogLevel::Warn,
                        &format!("pacman: ignoring corrupt index cache ({err}); rebuilding"),
                    ),
                },
                None => host.log(
                    LogLevel::Warn,
                    "pacman: index table cache entry is missing; rebuilding",
                ),
            },
            Err(err) => host.log(
                LogLevel::Warn,
                &format!("pacman: ignoring corrupt index cache ({err}); rebuilding"),
            ),
        }
    }

    let index = build_index(&local, root, level == "checksum", host)?;
    match persist(&base, &table_key, &records_key, &index, host) {
        Records::Cache(key) => Ok(Index {
            records: Records::Cache(key),
            ..index
        }),
        Records::Memory(_) => Ok(index),
    }
}

/// Writes the three index entries and reports how the built index should keep
/// its records (lazily from the cache, or in memory when a write failed).
fn persist(
    base: &str,
    table_key: &str,
    records_key: &str,
    index: &Index,
    host: &mut dyn Host,
) -> Records {
    let Records::Memory(records) = &index.records else {
        // Should not happen: a freshly built index always starts in memory.
        return Records::Cache(records_key.to_owned());
    };
    let table_ok = host.cache_set(table_key, &index.table).is_ok();
    let records_ok = host.cache_set(records_key, records).is_ok();
    let meta_ok = host.cache_set(base, &index.encode_meta()).is_ok();
    if table_ok && records_ok && meta_ok {
        Records::Cache(records_key.to_owned())
    } else {
        host.log(
            LogLevel::Debug,
            "pacman: cannot persist the index cache; keeping it in memory",
        );
        Records::Memory(Vec::new())
    }
}

/// The cache key for a database snapshot, crawl scope and check level.
///
/// Includes the format version, the database path, the crawl root, the check
/// level (which decides whether a checksum arena is built) and the `local/`
/// mtime, so a finished pacman transaction (which changes the directory) or a
/// different scan scope invalidates the index naturally.
pub fn cache_key(db_path: &Path, root: &str, level: &str, mtime: Option<SystemTime>) -> String {
    let stamp = mtime
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!(
        "pacman-index-v{INDEX_FORMAT}:{}:{stamp}:{root}:{level}",
        db_path.display()
    )
}

/// Accumulates packages, strings, records and the hash table while scanning.
struct Builder {
    packages: Vec<Package>,
    ids: HashMap<(String, String), u32>,
    strings: Vec<u8>,
    records: Vec<u8>,
    digests: Vec<u8>,
    digests_present: bool,
    /// Whether checksums are stored (`check-level = "checksum"`). The metadata
    /// level never builds the ~48-bytes-per-file digest arena.
    store_digests: bool,
    /// The open-addressed table, already in its serialized 12-bytes-per-slot
    /// form so the build never holds a second copy.
    table: Vec<u8>,
    count: u32,
    duplicates: u64,
}

impl Builder {
    /// Sizes the table for `estimate` paths (an upper bound; duplicates reduce
    /// the real load) so nothing has to be reallocated mid-build.
    fn new(estimate: usize, store_digests: bool) -> Self {
        let capacity = (estimate.saturating_mul(100) / TABLE_LOAD_PERCENT).max(16);
        Self {
            packages: Vec::new(),
            ids: HashMap::new(),
            strings: Vec::new(),
            // Reserve the upper bound exactly: a doubling realloc of a ~30 MiB
            // buffer is what pushed the build over the memory cap.
            records: Vec::with_capacity(estimate.saturating_mul(RECORD_LEN)),
            digests: Vec::new(),
            digests_present: false,
            store_digests,
            // Every slot starts empty (record index `NONE_U32`).
            table: vec![0xFF; capacity * SLOT_LEN],
            count: 0,
            duplicates: 0,
        }
    }

    fn slot(&self, position: usize) -> (u64, u32) {
        let offset = position * SLOT_LEN;
        (
            read_u64(&self.table, offset),
            read_u32(&self.table, offset + 8),
        )
    }

    fn intern(&mut self, name: &str, version: &str) -> u32 {
        let key = (name.to_owned(), version.to_owned());
        if let Some(&id) = self.ids.get(&key) {
            return id;
        }
        let id = self.packages.len() as u32;
        self.packages.push(Package {
            name: key.0.clone(),
            version: key.1.clone(),
        });
        self.ids.insert(key, id);
        id
    }

    /// Inserts one owned path. Returns `false` when it was a duplicate (the
    /// first claim wins, matching the previous behaviour for shared dirs).
    fn insert(&mut self, path: &str, owned: &OwnedPath) -> bool {
        let hash = path_hash(path);
        let capacity = self.table.len() / SLOT_LEN;
        let mut pos = (hash % capacity as u64) as usize;
        for _ in 0..capacity {
            let (slot_hash, slot_index) = self.slot(pos);
            if slot_index == NONE_U32 {
                break;
            }
            if slot_hash == hash {
                self.duplicates += 1;
                return false;
            }
            pos = (pos + 1) % capacity;
        }

        let package = self.intern(&owned.package, &owned.version);
        let record = encode_record(
            owned,
            package,
            self.store_digests,
            &mut self.strings,
            &mut self.digests,
            &mut self.digests_present,
        );
        let index = self.count;
        self.count += 1;
        self.records.extend_from_slice(&record);

        let offset = pos * SLOT_LEN;
        self.table[offset..offset + 8].copy_from_slice(&hash.to_le_bytes());
        self.table[offset + 8..offset + 12].copy_from_slice(&index.to_le_bytes());
        true
    }

    fn finish(self) -> Index {
        let capacity = self.table.len() / SLOT_LEN;
        let mut bloom = Bloom::for_entries(self.count as usize);
        for position in 0..capacity {
            let (hash, index) = self.slot(position);
            if index != NONE_U32 {
                bloom.insert(hash);
            }
        }
        let mut records = self.records;
        records.extend_from_slice(&self.digests);
        Index {
            format: INDEX_FORMAT,
            packages: self.packages,
            strings: self.strings,
            bloom,
            table: self.table,
            count: self.count,
            digests: self.digests_present,
            digest_count: (self.digests.len() / DIGEST_LEN) as u32,
            records: Records::Memory(records),
        }
    }
}

fn build_index(
    local: &Path,
    root: &str,
    store_digests: bool,
    host: &mut dyn Host,
) -> Result<Index, String> {
    let packages = package_dirs(local, host)?;

    // A cheap first pass over `files` (no gzip, no mtree) sizes the table.
    let mut estimate = 0usize;
    for package_dir in &packages {
        match relevant_paths(package_dir, root) {
            Ok(Some(paths)) => estimate += paths.len(),
            Ok(None) => {}
            Err(err) => host.log(
                LogLevel::Warn,
                &format!(
                    "pacman: skipping package directory {}: {err}",
                    package_dir.display()
                ),
            ),
        }
    }

    let mut builder = Builder::new(estimate, store_digests);
    for package_dir in packages {
        if let Err(err) = add_package(&mut builder, &package_dir, root, host) {
            host.log(
                LogLevel::Warn,
                &format!(
                    "pacman: skipping package directory {}: {err}",
                    package_dir.display()
                ),
            );
        }
    }
    if builder.duplicates > 0 {
        host.log(
            LogLevel::Debug,
            &format!(
                "pacman: {} paths are claimed by more than one package; keeping the first",
                builder.duplicates
            ),
        );
    }
    let index = builder.finish();
    host.log(
        LogLevel::Debug,
        &format!(
            "pacman: indexed {} paths under {root} from {}",
            index.len(),
            local.display()
        ),
    );
    Ok(index)
}

/// Lists the package directories under `local/`, sorted for determinism.
fn package_dirs(local: &Path, host: &mut dyn Host) -> Result<Vec<PathBuf>, String> {
    let reader = std::fs::read_dir(local).map_err(|err| {
        format!(
            "cannot read pacman database directory {}: {err}",
            local.display()
        )
    })?;
    let mut packages = Vec::new();
    for entry in reader {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                host.log(
                    LogLevel::Warn,
                    &format!("pacman: cannot read a database entry: {err}"),
                );
                continue;
            }
        };
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => packages.push(entry.path()),
            Ok(_) => {}
            Err(err) => host.log(
                LogLevel::Warn,
                &format!(
                    "pacman: cannot stat database entry {}: {err}",
                    entry.path().display()
                ),
            ),
        }
    }
    packages.sort();
    Ok(packages)
}

/// Reads a package's `files` list, keeping only paths at or below `root`.
///
/// Returns `None` when nothing is relevant, so `desc`/`mtree` (the bulk of the
/// init cost) are never touched for irrelevant packages.
fn relevant_paths(package_dir: &Path, root: &str) -> Result<Option<Vec<String>>, String> {
    let files_raw = std::fs::read(package_dir.join("files"))
        .map_err(|err| format!("cannot read files: {err}"))?;
    let files = parse_files(&files_raw).map_err(|err| format!("cannot parse files: {err}"))?;
    let files: Vec<String> = files
        .into_iter()
        .filter(|path| under_root(path, root))
        .collect();
    if files.is_empty() {
        Ok(None)
    } else {
        Ok(Some(files))
    }
}

/// Adds one package's owned paths to the builder.
///
/// The `mtree` is streamed entry by entry: only the current entry is ever
/// materialized, so a package with a multi-megabyte mtree (e.g. `breeze-icons`)
/// cannot spike the guest heap while the index accumulates. Files without a
/// matching mtree entry still become records (with no expected metadata), which
/// the detector reports as `unknown`.
fn add_package(
    builder: &mut Builder,
    package_dir: &Path,
    root: &str,
    host: &mut dyn Host,
) -> Result<(), String> {
    let Some(files) = relevant_paths(package_dir, root)? else {
        return Ok(());
    };

    let desc_raw = std::fs::read(package_dir.join("desc"))
        .map_err(|err| format!("cannot read desc: {err}"))?;
    let desc = parse_desc(&desc_raw).map_err(|err| format!("cannot parse desc: {err}"))?;
    let name = desc
        .name
        .ok_or_else(|| "desc has no %NAME% section".to_owned())?;
    let version = desc
        .version
        .ok_or_else(|| "desc has no %VERSION% section".to_owned())?;

    // Paths still waiting for their mtree entry; whatever is left afterwards
    // had no entry at all.
    let mut remaining: HashSet<String> = files.into_iter().collect();

    match std::fs::read(package_dir.join("mtree")) {
        Ok(raw) => match gunzip(&raw) {
            Ok(data) => {
                let mut unmatched = 0usize;
                visit_mtree(&data, |path, entry| {
                    // A path is inserted once, by its first mtree entry; a
                    // duplicate line (malformed, never produced by pacman) is
                    // ignored rather than overriding the first.
                    if remaining.remove(&path) {
                        builder.insert(
                            &path,
                            &OwnedPath {
                                package: name.clone(),
                                version: version.clone(),
                                backup: desc.backup.contains(&path),
                                mtree: Some(entry),
                            },
                        );
                    } else {
                        unmatched += 1;
                    }
                });
                if unmatched > 0 {
                    host.log(
                        LogLevel::Debug,
                        &format!(
                            "pacman: {name}: {unmatched} mtree entries have no matching `files` line"
                        ),
                    );
                }
            }
            Err(err) => host.log(
                LogLevel::Warn,
                &format!("pacman: {name}: cannot decompress mtree: {err}"),
            ),
        },
        Err(err) => host.log(
            LogLevel::Warn,
            &format!("pacman: {name}: cannot read mtree: {err}"),
        ),
    }

    for path in remaining {
        builder.insert(
            &path,
            &OwnedPath {
                package: name.clone(),
                version: version.clone(),
                backup: desc.backup.contains(&path),
                mtree: None,
            },
        );
    }
    Ok(())
}

/// Whether an absolute database path is inside the crawl root.
///
/// A crawl only asks about paths at or below `root`, so anything else would
/// never be looked up. `root` is already lexical-normalized and has no trailing
/// slash (except `/` itself).
fn under_root(path: &str, root: &str) -> bool {
    root == "/"
        || path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// A stable, dependency-free 64-bit hash (FNV-1a) of an absolute path.
///
/// The index is keyed by this hash, so it must not depend on the Rust version:
/// the standard `DefaultHasher` is explicitly unstable and would silently
/// invalidate every cached index.
fn path_hash(path: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in path.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Encodes one owned path into a fixed-size record, appending any variable
/// `link`/`invalid` strings and checksums to the shared arenas.
fn encode_record(
    owned: &OwnedPath,
    package: u32,
    store_digests: bool,
    strings: &mut Vec<u8>,
    digests: &mut Vec<u8>,
    digests_present: &mut bool,
) -> [u8; RECORD_LEN] {
    let mut record = [0u8; RECORD_LEN];
    let mut flags = 0u8;
    if owned.backup {
        flags |= 1 << 2;
    }

    let mut size = NONE_U64;
    let mut time = NONE_I64;
    let mut mode = NONE_U32;
    let mut uid = NONE_U32;
    let mut gid = NONE_U32;
    let mut link_off = NONE_U32;
    let mut link_len = 0u32;
    let mut invalid_off = NONE_U32;
    let mut invalid_len = 0u32;
    let mut digest_index = NONE_U32;

    if let Some(entry) = &owned.mtree {
        flags |= 1 << 3;
        flags |= match entry.kind {
            None => 0,
            Some(MtreeKind::File) => 1,
            Some(MtreeKind::Dir) => 2,
            Some(MtreeKind::Link) => 3,
        };
        size = entry.size.unwrap_or(NONE_U64);
        time = entry.time.unwrap_or(NONE_I64);
        mode = entry.mode.unwrap_or(NONE_U32);
        uid = entry.uid.unwrap_or(NONE_U32);
        gid = entry.gid.unwrap_or(NONE_U32);
        if let Some(link) = &entry.link {
            link_off = strings.len() as u32;
            link_len = link.len() as u32;
            strings.extend_from_slice(link.as_bytes());
        }
        if let Some(invalid) = &entry.invalid {
            invalid_off = strings.len() as u32;
            invalid_len = invalid.len() as u32;
            strings.extend_from_slice(invalid.as_bytes());
        }
        let md5 = store_digests
            .then_some(entry.md5.as_deref())
            .flatten()
            .and_then(|value| hex::decode(value).ok())
            .filter(|value| value.len() == 16);
        let sha = store_digests
            .then_some(entry.sha256.as_deref())
            .flatten()
            .and_then(|value| hex::decode(value).ok())
            .filter(|value| value.len() == 32);
        if md5.is_some() || sha.is_some() {
            if md5.is_some() {
                flags |= 1 << 4;
            }
            if sha.is_some() {
                flags |= 1 << 5;
            }
            digest_index = (digests.len() / DIGEST_LEN) as u32;
            let mut entry = [0u8; DIGEST_LEN];
            if let Some(md5) = md5 {
                entry[..16].copy_from_slice(&md5);
            }
            if let Some(sha) = sha {
                entry[16..].copy_from_slice(&sha);
            }
            digests.extend_from_slice(&entry);
            *digests_present = true;
        }
    }

    record[0..8].copy_from_slice(&size.to_le_bytes());
    record[8..16].copy_from_slice(&time.to_le_bytes());
    record[16..20].copy_from_slice(&mode.to_le_bytes());
    record[20..24].copy_from_slice(&uid.to_le_bytes());
    record[24..28].copy_from_slice(&gid.to_le_bytes());
    record[28..32].copy_from_slice(&package.to_le_bytes());
    record[32..36].copy_from_slice(&link_off.to_le_bytes());
    record[36..38].copy_from_slice(&(link_len.min(u16::MAX as u32) as u16).to_le_bytes());
    record[38..42].copy_from_slice(&invalid_off.to_le_bytes());
    record[42..44].copy_from_slice(&(invalid_len.min(u16::MAX as u32) as u16).to_le_bytes());
    record[44..48].copy_from_slice(&digest_index.to_le_bytes());
    record[48] = flags;
    record
}

fn opt_u32(value: u32) -> Option<u32> {
    (value != NONE_U32).then_some(value)
}

fn opt_u64(value: u64) -> Option<u64> {
    (value != NONE_U64).then_some(value)
}

fn opt_i64(value: i64) -> Option<i64> {
    (value != NONE_I64).then_some(value)
}

/// A bounds-checked little-endian reader over a byte slice.
struct Bytes<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bytes<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(len)
            .filter(|end| *end <= self.data.len())
            .ok_or_else(|| "truncated index cache entry".to_owned())?;
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u16(&mut self) -> Result<u16, String> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, String> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Result<u64, String> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn read_i64(bytes: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn put_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn gunzip(raw: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoder = GzDecoder::new(raw);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|err| err.to_string())?;
    Ok(out)
}

/// Parses the `files` database file into absolute paths.
pub fn parse_files(bytes: &[u8]) -> Result<Vec<String>, String> {
    let text = String::from_utf8_lossy(bytes);
    // Meta packages (`base`, `base-devel`, ...) ship an empty `files` file: no
    // paths, not a corrupt database.
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut lines = text.lines();
    let header = lines
        .next()
        .unwrap_or_default()
        .trim_end_matches('\r')
        .trim_start_matches('\u{feff}');
    if header != "%FILES%" {
        return Err(format!(
            "missing %FILES% header (first line was {header:?})"
        ));
    }

    let mut paths = Vec::new();
    let mut seen = BTreeSet::new();
    for line in lines {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        // Directories carry a trailing `/`, which is metadata rather than part
        // of the path; the mtree entry decides the type.
        let trimmed = line.trim_end_matches('/');
        let trimmed = if trimmed.is_empty() { "/" } else { trimmed };
        let absolute = absolute_db_path(trimmed);
        if seen.insert(absolute.clone()) {
            paths.push(absolute);
        }
    }
    Ok(paths)
}

/// The result of parsing a `desc` file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Desc {
    /// `%NAME%` (first line).
    pub name: Option<String>,
    /// `%VERSION%` (first line).
    pub version: Option<String>,
    /// `%BACKUP%` paths, normalized to absolute paths.
    pub backup: BTreeSet<String>,
}

/// The result of parsing an mtree file into a map (test helper; the detector
/// streams with [`visit_mtree`] instead).
#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub struct Mtree {
    /// Parsed entries keyed by absolute path.
    pub entries: BTreeMap<String, MtreeEntry>,
    /// Diagnostics for lines that could not be attributed to a path. Entries
    /// that can be attributed carry their problem in [`MtreeEntry::invalid`].
    pub errors: Vec<String>,
}

/// Parses a `desc` file.
pub fn parse_desc(bytes: &[u8]) -> Result<Desc, String> {
    let text = String::from_utf8_lossy(bytes);
    let mut desc = Desc::default();
    let mut section: Option<String> = None;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with('%') && line.ends_with('%') && line.len() > 1 {
            section = Some(line.to_owned());
            continue;
        }
        if line.is_empty() {
            section = None;
            continue;
        }
        match section.as_deref() {
            Some("%NAME%") if desc.name.is_none() => desc.name = Some(line.to_owned()),
            Some("%VERSION%") if desc.version.is_none() => desc.version = Some(line.to_owned()),
            Some("%BACKUP%") => {
                let trimmed = line.trim_end_matches('/');
                let trimmed = if trimmed.is_empty() { "/" } else { trimmed };
                desc.backup.insert(absolute_db_path(trimmed));
            }
            _ => {}
        }
    }
    Ok(desc)
}

/// Parses a plain (decompressed) BSD mtree into a map.
///
/// The detector streams with [`visit_mtree`] instead; this collecting form is
/// used by the parser tests.
#[cfg(test)]
pub fn parse_mtree(bytes: &[u8]) -> Mtree {
    let mut out = Mtree::default();
    visit_mtree(bytes, |path, entry| {
        if out.entries.insert(path.clone(), entry).is_some() {
            out.errors.push(format!("duplicate entry for `{path}`"));
        }
    });
    out
}

/// Streams a plain (decompressed) BSD mtree, calling `visit` for every entry.
///
/// `/set` and `/unset` update the defaults; entries are yielded one at a time
/// (only the current [`MtreeEntry`] is materialized), so a large mtree does not
/// have to be held in memory as a map. `path` is an absolute `/...` key and any
/// problem is carried in [`MtreeEntry::invalid`].
pub fn visit_mtree(bytes: &[u8], mut visit: impl FnMut(String, MtreeEntry)) {
    let mut defaults: BTreeMap<String, String> = BTreeMap::new();

    for raw_line in bytes.split(|byte| *byte == b'\n') {
        let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        if line.is_empty() || line[0] == b'#' {
            continue;
        }
        let tokens = tokenize(line);
        let Some(first) = tokens.first() else {
            continue;
        };
        let keyword = String::from_utf8_lossy(first);
        match &*keyword {
            "/set" => {
                for token in &tokens[1..] {
                    if let Some((key, value)) = field(token) {
                        defaults.insert(key.to_owned(), value.to_owned());
                    }
                }
                continue;
            }
            "/unset" => {
                for token in &tokens[1..] {
                    let key = String::from_utf8_lossy(token);
                    defaults.remove(key.as_ref());
                }
                continue;
            }
            _ => {}
        }

        let Some(path) = mtree_path(first) else {
            continue;
        };
        let entry = parse_entry(&tokens[1..], &defaults);
        visit(path, entry);
    }
}

fn parse_entry(tokens: &[Vec<u8>], defaults: &BTreeMap<String, String>) -> MtreeEntry {
    let mut fields: BTreeMap<String, String> = defaults.clone();
    for token in tokens {
        if let Some((key, value)) = field(token) {
            fields.insert(key.to_owned(), value.to_owned());
        }
    }

    let mut entry = MtreeEntry::default();
    match fields.get("type").map(String::as_str) {
        Some("file") => entry.kind = Some(MtreeKind::File),
        Some("dir") => entry.kind = Some(MtreeKind::Dir),
        Some("link") => entry.kind = Some(MtreeKind::Link),
        Some(other) => entry.invalidate(format!("unknown mtree type `{other}`")),
        None => entry.invalidate("mtree entry has no type"),
    }

    if let Some(value) = fields.get("size") {
        match value.parse::<u64>() {
            Ok(size) => entry.size = Some(size),
            Err(_) => entry.invalidate(format!("invalid size `{value}`")),
        }
    }
    if let Some(value) = fields.get("time") {
        let seconds = value.split('.').next().unwrap_or(value);
        match seconds.parse::<i64>() {
            Ok(time) => entry.time = Some(time),
            Err(_) => entry.invalidate(format!("invalid time `{value}`")),
        }
    }
    if let Some(value) = fields.get("mode") {
        match u32::from_str_radix(value, 8) {
            Ok(mode) => entry.mode = Some(mode),
            Err(_) => entry.invalidate(format!("invalid mode `{value}`")),
        }
    }
    if let Some(value) = fields.get("uid") {
        match value.parse::<u32>() {
            Ok(uid) => entry.uid = Some(uid),
            Err(_) => entry.invalidate(format!("invalid uid `{value}`")),
        }
    }
    if let Some(value) = fields.get("gid") {
        match value.parse::<u32>() {
            Ok(gid) => entry.gid = Some(gid),
            Err(_) => entry.invalidate(format!("invalid gid `{value}`")),
        }
    }
    if let Some(value) = fields.get("link") {
        entry.link = Some(value.clone());
    }
    if let Some(value) = fields.get("md5digest") {
        if is_hex(value, 32) {
            entry.md5 = Some(value.to_ascii_lowercase());
        } else {
            entry.invalidate(format!("invalid md5digest `{value}`"));
        }
    }
    if let Some(value) = fields.get("sha256digest") {
        if is_hex(value, 64) {
            entry.sha256 = Some(value.to_ascii_lowercase());
        } else {
            entry.invalidate(format!("invalid sha256digest `{value}`"));
        }
    }

    if entry.kind == Some(MtreeKind::Link) && entry.link.is_none() {
        entry.invalidate("link entry has no link target");
    }
    entry
}

fn is_hex(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Splits an mtree line into whitespace-delimited tokens, decoding octal and
/// backslash escapes. Escaped whitespace stays inside its token.
fn tokenize(line: &[u8]) -> Vec<Vec<u8>> {
    let mut tokens = Vec::new();
    let mut current = Vec::new();
    let mut index = 0;
    while index < line.len() {
        let byte = line[index];
        if byte.is_ascii_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            index += 1;
            continue;
        }
        if byte == b'\\' {
            if index + 3 < line.len()
                && line[index + 1].is_ascii_digit()
                && line[index + 2].is_ascii_digit()
                && line[index + 3].is_ascii_digit()
            {
                let value = (line[index + 1] - b'0') * 64
                    + (line[index + 2] - b'0') * 8
                    + (line[index + 3] - b'0');
                current.push(value);
                index += 4;
                continue;
            }
            if index + 1 < line.len() {
                current.push(line[index + 1]);
                index += 2;
                continue;
            }
        }
        current.push(byte);
        index += 1;
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Splits a decoded `key=value` token.
fn field(token: &[u8]) -> Option<(&str, &str)> {
    let text = std::str::from_utf8(token).ok()?;
    let (key, value) = text.split_once('=')?;
    if key.is_empty() {
        return None;
    }
    Some((key, value))
}

/// Converts an mtree/`files` relative path to an absolute `/...` key.
fn mtree_path(token: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(token);
    let relative = text.strip_prefix("./").unwrap_or(&text);
    if relative.is_empty() || relative == "." {
        return None;
    }
    Some(absolute_db_path(relative))
}

/// Normalizes a database path to an absolute key (`usr/bin/foo` → `/usr/bin/foo`).
pub fn absolute_db_path(raw: &str) -> String {
    let trimmed = raw.trim_end_matches('/');
    let trimmed = if trimmed.is_empty() { "/" } else { trimmed };
    if trimmed.starts_with('/') {
        trimmed.to_owned()
    } else {
        format!("/{trimmed}")
    }
}

/// Lexically normalizes a path (`.` and `..` components, no filesystem access).
pub fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                // Never climb above the root.
                if !out.pop() {
                    out.push(Path::new("/"));
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    use backupcrawl_plugin_sdk::MockHost;

    fn gzip(data: &str) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data.as_bytes()).unwrap();
        encoder.finish().unwrap()
    }

    fn entry(kind: Option<MtreeKind>, size: Option<u64>) -> MtreeEntry {
        MtreeEntry {
            kind,
            size,
            ..MtreeEntry::default()
        }
    }

    #[test]
    fn files_parser_preserves_paths_and_normalizes_absolute() {
        let files = b"%FILES%\nusr/bin/foo\nusr/share/my app/config file\netc/\n/opt/x\n\n";
        let parsed = parse_files(files).unwrap();
        assert_eq!(
            parsed,
            vec![
                "/usr/bin/foo",
                "/usr/share/my app/config file",
                "/etc",
                "/opt/x",
            ]
        );
    }

    #[test]
    fn files_parser_accepts_an_empty_meta_package() {
        assert!(parse_files(b"").unwrap().is_empty());
        assert!(parse_files(b"\n\n").unwrap().is_empty());
    }

    #[test]
    fn files_parser_requires_the_header() {
        let err = parse_files(b"usr/bin/foo\n").unwrap_err();
        assert!(err.contains("%FILES%"), "{err}");
    }

    #[test]
    fn desc_parser_reads_name_version_and_backup_paths() {
        let desc =
            b"%NAME%\nfilesystem\n\n%VERSION%\n2025.01-1\n\n%BACKUP%\netc/passwd\netc/group\n";
        let parsed = parse_desc(desc).unwrap();
        assert_eq!(parsed.name.as_deref(), Some("filesystem"));
        assert_eq!(parsed.version.as_deref(), Some("2025.01-1"));
        assert_eq!(
            parsed.backup,
            BTreeSet::from(["/etc/passwd".to_owned(), "/etc/group".to_owned()])
        );
    }

    #[test]
    fn mtree_defaults_and_entries_are_parsed() {
        let mtree = b"#mtree\n\
/set type=file uid=0 gid=0 mode=644\n\
./usr/bin/foo time=1700000000.0 size=12 md5digest=0123456789abcdef0123456789abcdef\n\
./usr/bin type=dir\n\
./usr/bin/bar type=link link=foo mode=777\n";
        let parsed = parse_mtree(mtree);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);

        let foo = &parsed.entries["/usr/bin/foo"];
        assert_eq!(foo.kind, Some(MtreeKind::File));
        assert_eq!(foo.uid, Some(0));
        assert_eq!(foo.gid, Some(0));
        assert_eq!(foo.mode, Some(0o644));
        assert_eq!(foo.size, Some(12));
        assert_eq!(foo.time, Some(1_700_000_000));
        assert_eq!(foo.md5.as_deref(), Some("0123456789abcdef0123456789abcdef"));
        assert_eq!(foo.invalid, None);

        let dir = &parsed.entries["/usr/bin"];
        assert_eq!(dir.kind, Some(MtreeKind::Dir));
        assert_eq!(dir.mode, Some(0o644), "default mode applies");

        let link = &parsed.entries["/usr/bin/bar"];
        assert_eq!(link.kind, Some(MtreeKind::Link));
        assert_eq!(link.link.as_deref(), Some("foo"));
        assert_eq!(link.mode, Some(0o777));
    }

    #[test]
    fn mtree_octal_escapes_decode_spaces_and_backslashes() {
        let parsed =
            parse_mtree(b"./usr/share/my\\040app/back\\134slash time=1 size=1 type=file\n");
        let entry = parsed
            .entries
            .get("/usr/share/my app/back\\slash")
            .expect("both escapes decoded");
        assert_eq!(entry.kind, Some(MtreeKind::File));
        assert_eq!(entry.invalid, None);
    }

    #[test]
    fn mtree_unset_removes_a_default() {
        let mtree = b"/set type=file mode=644\n./a time=1 size=1\n/unset mode\n./b type=dir\n";
        let parsed = parse_mtree(mtree);
        assert_eq!(parsed.entries["/a"].mode, Some(0o644));
        assert_eq!(parsed.entries["/b"].mode, None);
    }

    #[test]
    fn mtree_unknown_type_is_flagged_not_guessed() {
        let parsed = parse_mtree(b"./odd type=fifo\n./bad mode=abc\n");
        assert_eq!(parsed.entries["/odd"].kind, None);
        assert!(
            parsed.entries["/odd"]
                .invalid
                .as_deref()
                .unwrap()
                .contains("fifo"),
            "{:?}",
            parsed.entries["/odd"]
        );
        assert!(
            parsed.entries["/bad"]
                .invalid
                .as_deref()
                .unwrap()
                .contains("mode"),
            "{:?}",
            parsed.entries["/bad"]
        );
    }

    #[test]
    fn mtree_link_without_target_is_flagged() {
        let parsed = parse_mtree(b"./l type=link\n");
        assert!(parsed.entries["/l"].invalid.is_some());
    }

    #[test]
    fn gzip_round_trip_feeds_the_parser() {
        let data = gzip("./a type=file size=1 time=1\n");
        let plain = gunzip(&data).unwrap();
        assert_eq!(plain, b"./a type=file size=1 time=1\n");
        assert_eq!(
            parse_mtree(&plain).entries["/a"].kind,
            Some(MtreeKind::File)
        );
    }

    #[test]
    fn lexical_normalize_resolves_dot_and_dotdot() {
        assert_eq!(
            lexical_normalize(Path::new("/a/./b/../c")),
            PathBuf::from("/a/c")
        );
        assert_eq!(lexical_normalize(Path::new("/..")), PathBuf::from("/"));
        assert_eq!(
            lexical_normalize(Path::new("/a/b/../../../x")),
            PathBuf::from("/x")
        );
    }

    #[test]
    fn index_round_trips_through_meta_and_lazy_records() {
        let mut entries = BTreeMap::new();
        entries.insert(
            "/usr/bin/demo".to_owned(),
            OwnedPath {
                package: "demo".to_owned(),
                version: "1.0-1".to_owned(),
                backup: false,
                mtree: Some(entry(Some(MtreeKind::File), Some(5))),
            },
        );
        entries.insert(
            "/usr/lib/libx.so".to_owned(),
            OwnedPath {
                package: "libx".to_owned(),
                version: "2.0-1".to_owned(),
                backup: true,
                mtree: Some(MtreeEntry {
                    kind: Some(MtreeKind::Link),
                    link: Some("/usr/lib/libx.so.1".to_owned()),
                    ..MtreeEntry::default()
                }),
            },
        );
        let index = Index::from_entries(&entries);

        // Reassemble the index exactly as `load_index` would from the cache:
        // meta + table in memory, records read one at a time.
        let meta = index.encode_meta();
        let records = match &index.records {
            Records::Memory(bytes) => bytes.clone(),
            Records::Cache(_) => unreachable!(),
        };
        let mut host = MockHost::new().with_cache("records", records);
        let mut cached =
            Index::from_meta(&meta, index.table.clone(), "records".to_owned()).expect("valid meta");

        let file = cached.lookup("/usr/bin/demo", &mut host).expect("owned");
        assert_eq!(file.package, "demo");
        assert_eq!(file.mtree.unwrap().size, Some(5));
        let link = cached.lookup("/usr/lib/libx.so", &mut host).expect("owned");
        assert_eq!(
            link.mtree.unwrap().link.as_deref(),
            Some("/usr/lib/libx.so.1")
        );
        assert!(link.backup);

        assert!(cached.lookup("/usr/bin/other", &mut host).is_none());
        assert!(host.cache_reads().len() >= 2, "records read on demand");
    }

    #[test]
    fn duplicate_paths_keep_the_first_package() {
        let mut builder = Builder::new(2, false);
        let first = OwnedPath {
            package: "first".to_owned(),
            version: "1".to_owned(),
            backup: false,
            mtree: Some(entry(Some(MtreeKind::File), Some(1))),
        };
        let second = OwnedPath {
            package: "second".to_owned(),
            version: "1".to_owned(),
            backup: false,
            mtree: Some(entry(Some(MtreeKind::File), Some(2))),
        };
        assert!(builder.insert("/usr", &first));
        assert!(!builder.insert("/usr", &second));
        let index = builder.finish();
        assert_eq!(index.len(), 1);

        let mut host = MockHost::new();
        let mut index = index;
        let owned = index.lookup("/usr", &mut host).unwrap();
        assert_eq!(owned.package, "first");
    }
}
