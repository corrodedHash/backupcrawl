//! Native parsing of the pacman local database.
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
//! [`load_index`] builds one deterministic path → package/mtree index,
//! restricted to the paths the scan can ask about (the crawl root and below),
//! and caches it through the host cache, keyed by the database path, the
//! `local/` directory mtime and the crawl root. Restricting the index keeps a
//! scan of `~` or a project tree from parsing the whole system database. A
//! corrupt cache entry is ignored and rebuilt; an unreadable database is an
//! `init` error (the host disables the plugin with a visible note). Unparseable
//! *entries* never become a wrong `clean`: they are flagged and the detector
//! reports `unknown`.
//!
//! The database format is stable but not a public API. This parser knows the
//! fields the detector needs and ignores everything else.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use backupcrawl_plugin_sdk::{Host, LogLevel};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};

/// Version of the serialized cache format. Bump when [`Index`] changes or the
/// index scope semantics change.
pub const INDEX_FORMAT: u32 = 2;

/// The package that owns a path and its expected mtree entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedPath {
    /// Package name (`desc` `%NAME%`).
    pub package: String,
    /// Package version (`desc` `%VERSION%`).
    pub version: String,
    /// Whether the path is listed in `%BACKUP%`.
    #[serde(default)]
    pub backup: bool,
    /// Expected mtree entry, or `None` when the package has no usable entry
    /// for this path (missing/unreadable mtree, or a `files` line without a
    /// matching mtree line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtree: Option<MtreeEntry>,
}

/// The deterministic path → package/mtree index for one database snapshot.
///
/// Only paths at or below the scan's crawl root are present; the detector is
/// never asked about anything else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    /// Cache format version.
    pub format: u32,
    /// Owned paths, keyed by absolute path.
    pub entries: BTreeMap<String, OwnedPath>,
}

impl Index {
    /// An empty index in the current format.
    pub fn empty() -> Self {
        Self {
            format: INDEX_FORMAT,
            entries: BTreeMap::new(),
        }
    }

    /// Number of owned paths.
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

impl Default for Index {
    fn default() -> Self {
        Self::empty()
    }
}

/// Entry type from the mtree `type` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MtreeKind {
    /// Regular file.
    File,
    /// Directory.
    Dir,
    /// Symbolic link.
    Link,
}

/// One parsed mtree entry. Optional fields are only compared when present.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MtreeEntry {
    /// Expected object type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<MtreeKind>,
    /// Expected size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Expected modification time, seconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<i64>,
    /// Expected permission bits (`0o7777`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    /// Expected owner uid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    /// Expected owner gid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
    /// Expected symlink target, verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Expected MD5 digest (lowercase hex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
    /// Expected SHA-256 digest (lowercase hex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Why this entry cannot be trusted; a claim on it is `unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
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

/// The result of parsing an mtree file.
#[derive(Debug, Clone, Default)]
pub struct Mtree {
    /// Parsed entries keyed by absolute path.
    pub entries: BTreeMap<String, MtreeEntry>,
    /// Diagnostics for lines that could not be attributed to a path. Entries
    /// that can be attributed carry their problem in [`MtreeEntry::invalid`].
    pub errors: Vec<String>,
}

/// Builds (or loads from the host cache) the index for `<db_path>/local`,
/// restricted to `root` and its descendants.
pub fn load_index(db_path: &Path, root: &str, host: &mut dyn Host) -> Result<Index, String> {
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

    let key = cache_key(db_path, root, metadata.modified().ok());
    if let Some(bytes) = host.cache_get(&key) {
        match serde_json::from_slice::<Index>(&bytes) {
            Ok(index) if index.format == INDEX_FORMAT => {
                host.log(
                    LogLevel::Debug,
                    &format!("pacman: index cache hit ({} paths)", index.len()),
                );
                return Ok(index);
            }
            Ok(index) => host.log(
                LogLevel::Warn,
                &format!(
                    "pacman: ignoring cached index in unknown format {}; rebuilding",
                    index.format
                ),
            ),
            Err(err) => host.log(
                LogLevel::Warn,
                &format!("pacman: ignoring corrupt index cache ({err}); rebuilding"),
            ),
        }
    }

    let index = build_index(&local, root, host)?;
    match serde_json::to_vec(&index) {
        Ok(bytes) => {
            if let Err(err) = host.cache_set(&key, &bytes) {
                host.log(
                    LogLevel::Debug,
                    &format!("pacman: cannot write index cache: {err}"),
                );
            }
        }
        Err(err) => host.log(
            LogLevel::Warn,
            &format!("pacman: cannot serialize index cache: {err}"),
        ),
    }
    Ok(index)
}

/// The cache key for a database snapshot and crawl scope.
///
/// Includes the format version, the database path, the crawl root and the
/// `local/` mtime, so a finished pacman transaction (which changes the
/// directory) or a different scan scope invalidates the index naturally.
pub fn cache_key(db_path: &Path, root: &str, mtime: Option<SystemTime>) -> String {
    let stamp = mtime
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!(
        "pacman-index-v{INDEX_FORMAT}:{}:{stamp}:{root}",
        db_path.display()
    )
}

fn build_index(local: &Path, root: &str, host: &mut dyn Host) -> Result<Index, String> {
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

    let mut index = Index {
        format: INDEX_FORMAT,
        entries: BTreeMap::new(),
    };
    let mut duplicates = 0usize;
    for package_dir in packages {
        match read_package(&package_dir, root, host) {
            Ok(Some(paths)) => {
                for (path, owned) in paths {
                    if index.entries.contains_key(&path) {
                        // Shared directories are normal (every package owns
                        // `/usr`, `/usr/bin`, ...); counting them keeps the log
                        // quiet instead of warning once per shared path.
                        duplicates += 1;
                        continue;
                    }
                    index.entries.insert(path, owned);
                }
            }
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
    if duplicates > 0 {
        host.log(
            LogLevel::Debug,
            &format!(
                "pacman: {duplicates} paths are claimed by more than one package; keeping the first"
            ),
        );
    }
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

/// Reads one package directory into `(absolute path, owned path)` pairs.
///
/// Returns `None` when the package owns no path at or below `root`; the `files`
/// list is read first so irrelevant packages never pay for `desc`/`mtree`
/// parsing (the bulk of the init cost).
fn read_package(
    package_dir: &Path,
    root: &str,
    host: &mut dyn Host,
) -> Result<Option<Vec<(String, OwnedPath)>>, String> {
    let files_raw = std::fs::read(package_dir.join("files"))
        .map_err(|err| format!("cannot read files: {err}"))?;
    let files = parse_files(&files_raw).map_err(|err| format!("cannot parse files: {err}"))?;
    let files: Vec<String> = files
        .into_iter()
        .filter(|path| under_root(path, root))
        .collect();
    if files.is_empty() {
        return Ok(None);
    }

    let desc_raw = std::fs::read(package_dir.join("desc"))
        .map_err(|err| format!("cannot read desc: {err}"))?;
    let desc = parse_desc(&desc_raw).map_err(|err| format!("cannot parse desc: {err}"))?;
    let name = desc
        .name
        .ok_or_else(|| "desc has no %NAME% section".to_owned())?;
    let version = desc
        .version
        .ok_or_else(|| "desc has no %VERSION% section".to_owned())?;

    let mtree = match std::fs::read(package_dir.join("mtree")) {
        Ok(raw) => match gunzip(&raw) {
            Ok(data) => {
                let parsed = parse_mtree(&data);
                for error in parsed.errors.iter().take(3) {
                    host.log(LogLevel::Warn, &format!("pacman: {name}: mtree: {error}"));
                }
                Some(parsed.entries)
            }
            Err(err) => {
                host.log(
                    LogLevel::Warn,
                    &format!("pacman: {name}: cannot decompress mtree: {err}"),
                );
                None
            }
        },
        Err(err) => {
            host.log(
                LogLevel::Warn,
                &format!("pacman: {name}: cannot read mtree: {err}"),
            );
            None
        }
    };

    let mut out = Vec::with_capacity(files.len());
    for path in files {
        let entry = mtree
            .as_ref()
            .and_then(|entries| entries.get(&path))
            .cloned();
        let backup = desc.backup.contains(&path);
        out.push((
            path,
            OwnedPath {
                package: name.clone(),
                version: version.clone(),
                backup,
                mtree: entry,
            },
        ));
    }
    Ok(Some(out))
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

/// Parses a plain (decompressed) BSD mtree.
pub fn parse_mtree(bytes: &[u8]) -> Mtree {
    let mut out = Mtree::default();
    let mut defaults: BTreeMap<String, String> = BTreeMap::new();

    for (number, raw_line) in bytes.split(|byte| *byte == b'\n').enumerate() {
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
        if out.entries.insert(path.clone(), entry).is_some() {
            out.errors
                .push(format!("line {}: duplicate entry for `{path}`", number + 1));
        }
    }
    out
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

    fn gzip(data: &str) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data.as_bytes()).unwrap();
        encoder.finish().unwrap()
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
}
