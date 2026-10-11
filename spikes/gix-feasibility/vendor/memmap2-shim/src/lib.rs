//! Minimal `memmap2`-compatible shim for wasm32-wasip2.
//!
//! `memmap2` 0.9 has no wasm implementation: on unsupported platforms its
//! `MmapOptions::map*` return `ErrorKind::Unsupported`. gix 0.89 uses mmap
//! unconditionally for the index (gix-index), loose objects (gix-odb), pack
//! data/index files (gix-pack) and large packed-refs buffers (gix-ref), and
//! has no feature to turn that off anymore (`gix-features/mmap` is gone).
//!
//! This shim replaces the mapping with an eager read into a `Vec<u8>` and
//! implements the subset of the memmap2 API used by those crates. It is wired
//! in through `[patch.crates-io]` in the spike workspace.
//!
//! Caveats (documented in docs/rust-rewrite-plan.md §7.1):
//! - Whole files are read eagerly; for very large packs this costs memory
//!   proportional to the pack size (acceptable for M1, revisit if needed).
//! - Writes are not mapped: `make_mut`/`flush` are no-ops. gix only reads
//!   in this detector, so that is fine.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::ops::Deref;

#[derive(Clone, Copy, Debug, Default)]
pub struct MmapOptions {
    len: Option<usize>,
    offset: u64,
    populate: bool,
    stack: bool,
}

impl MmapOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&mut self, len: usize) -> &mut Self {
        self.len = Some(len);
        self
    }

    pub fn offset(&mut self, offset: u64) -> &mut Self {
        self.offset = offset;
        self
    }

    pub fn stack(&mut self) -> &mut Self {
        self.stack = true;
        self
    }

    pub fn populate(&mut self) -> &mut Self {
        self.populate = true;
        self
    }

    pub fn map(&self, file: &File) -> io::Result<Mmap> {
        Mmap::read(file, self)
    }

    pub fn map_copy(&self, file: &File) -> io::Result<Mmap> {
        Mmap::read(file, self)
    }

    pub fn map_copy_read_only(&self, file: &File) -> io::Result<Mmap> {
        Mmap::read(file, self)
    }

    pub fn map_exec(&self, file: &File) -> io::Result<Mmap> {
        Mmap::read(file, self)
    }

    pub fn map_mut(&self, file: &File) -> io::Result<Mmap> {
        Mmap::read(file, self)
    }

    pub fn map_anon(&self, len: usize) -> io::Result<Mmap> {
        Ok(Mmap { data: vec![0; len] })
    }
}

/// A read-only memory map stand-in backed by an owned buffer.
pub struct Mmap {
    data: Vec<u8>,
}

// SAFETY: like the real `memmap2::Mmap`, the bytes are immutable from the
// guest's point of view; the shim just owns them.
unsafe impl Send for Mmap {}
unsafe impl Sync for Mmap {}

impl Mmap {
    fn read(file: &File, options: &MmapOptions) -> io::Result<Self> {
        let mut file = file;
        if options.offset != 0 {
            file.seek(SeekFrom::Start(options.offset))?;
        }
        let data = match options.len {
            Some(len) => {
                let mut data = vec![0; len];
                file.read_exact(&mut data)?;
                data
            }
            None => {
                let mut data = Vec::new();
                file.read_to_end(&mut data)?;
                data
            }
        };
        Ok(Mmap { data })
    }

    pub unsafe fn map(file: &File) -> io::Result<Mmap> {
        MmapOptions::new().map(file)
    }

    pub unsafe fn map_exec(file: &File) -> io::Result<Mmap> {
        MmapOptions::new().map_exec(file)
    }

    pub unsafe fn map_mut(file: &File) -> io::Result<Mmap> {
        MmapOptions::new().map_mut(file)
    }

    pub unsafe fn map_copy(file: &File) -> io::Result<Mmap> {
        MmapOptions::new().map_copy(file)
    }

    pub unsafe fn map_copy_read_only(file: &File) -> io::Result<Mmap> {
        MmapOptions::new().map_copy_read_only(file)
    }

    pub fn map_anon(len: usize) -> io::Result<Mmap> {
        MmapOptions::new().map_anon(len)
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn ptr(&self) -> *const u8 {
        self.data.as_ptr()
    }

    pub fn mut_ptr(&mut self) -> *mut u8 {
        self.data.as_mut_ptr()
    }

    pub fn advise(&self, _advice: Advice) -> io::Result<()> {
        Ok(())
    }

    pub fn advise_range(&self, _advice: Advice, _offset: usize, _len: usize) -> io::Result<()> {
        Ok(())
    }

    pub fn lock(&self) -> io::Result<()> {
        Ok(())
    }

    pub fn unlock(&self) -> io::Result<()> {
        Ok(())
    }

    pub fn make_read_only(&mut self) -> io::Result<()> {
        Ok(())
    }

    pub fn make_exec(&mut self) -> io::Result<()> {
        Ok(())
    }

    pub fn make_mut(&mut self) -> io::Result<()> {
        Ok(())
    }

    pub fn flush(&self) -> io::Result<()> {
        Ok(())
    }

    pub fn flush_async(&self) -> io::Result<()> {
        Ok(())
    }

    pub fn flush_range(&self, _offset: usize, _len: usize) -> io::Result<()> {
        Ok(())
    }
}

impl Deref for Mmap {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.data
    }
}

impl std::fmt::Debug for Mmap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mmap").field("len", &self.data.len()).finish()
    }
}

impl AsRef<[u8]> for Mmap {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

/// The subset of `memmap2::Advice` used by consumers of this shim.
#[derive(Clone, Copy, Debug)]
pub enum Advice {
    Normal,
    Random,
    Sequential,
    WillNeed,
    DontNeed,
    NoHugePages,
    HugePage,
    DontFork,
    DoFork,
    Unmergeable,
}

pub fn file_len(file: &File) -> io::Result<u64> {
    Ok(file.metadata()?.len())
}
