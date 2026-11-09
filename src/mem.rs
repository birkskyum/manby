//! Memory: how much the process uses.

/// The resident set size of the process in bytes, if the platform says.
pub fn resident() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
        // SAFETY: info is a proc_taskinfo of the size given.
        let n = unsafe {
            libc::proc_pidinfo(
                libc::getpid(),
                libc::PROC_PIDTASKINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        if n == size {
            return Some(info.pti_resident_size);
        }
        None
    }
    #[cfg(target_os = "linux")]
    {
        let s = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = s.split_whitespace().nth(1)?.parse().ok()?;
        // SAFETY: sysconf has no preconditions.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
        Some(pages * page)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// Lets the pages of part of a file mapping go: they are read from the file
/// again if they are needed, but no longer count as the process's memory.
pub fn release_mapped(part: &[u8]) {
    // SAFETY: sysconf has no preconditions.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let start = (part.as_ptr() as usize).next_multiple_of(page);
    let end = (part.as_ptr() as usize + part.len()) / page * page;
    if end <= start {
        return;
    }
    let (addr, len) = (start as *mut libc::c_void, end - start);
    // SAFETY: the range lies inside a read-only mapping of a file, whose
    // contents these calls leave as they are.
    unsafe {
        #[cfg(target_os = "macos")]
        libc::msync(addr, len, libc::MS_INVALIDATE);
        #[cfg(not(target_os = "macos"))]
        libc::madvise(addr, len, libc::MADV_DONTNEED);
    }
}

/// The peak resident set size of the process in bytes.
pub fn peak_resident() -> Option<u64> {
    // SAFETY: rusage is plain data that getrusage fills in.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: ru is a valid rusage.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) } != 0 {
        return None;
    }
    let max = ru.ru_maxrss as u64;
    // bytes on macOS, kilobytes elsewhere
    if cfg!(target_os = "macos") {
        Some(max)
    } else {
        Some(max * 1024)
    }
}

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use tippecanoe::errors::{EXIT_OPEN, EXIT_READ, EXIT_WRITE};

use crate::Error;
use crate::buf::Buf;

/// How many bytes a spool collects in one buffer.
const REGION: usize = 256 << 10;
/// Writers go to one of this many buffers, by thread.
const SPOOL_SHARDS: usize = 8;
/// Regions read back from the file that a spool keeps for the next reads.
const CACHED: usize = 64;
const NO_SHARD: u32 = u32::MAX;

/// How many bytes of data at rest may stay in memory; the rest goes to
/// temporary files.
pub struct Rest {
    used: AtomicU64,
    allowance: u64,
}

impl Rest {
    pub fn new(allowance: usize) -> Arc<Rest> {
        Arc::new(Rest {
            used: AtomicU64::new(0),
            allowance: allowance as u64,
        })
    }

    /// Takes room for `n` bytes, if there is room.
    fn take(&self, n: usize) -> bool {
        let n = n as u64;
        self.used
            .fetch_update(Relaxed, Relaxed, |u| {
                (u + n <= self.allowance).then_some(u + n)
            })
            .is_ok()
    }

    fn give(&self, n: usize) {
        self.used.fetch_sub(n as u64, Relaxed);
    }
}

/// Data at rest: written once and read back later, kept in memory in large
/// blocks while `Rest` has room and otherwise in an unlinked temporary file,
/// whose pages are in the page cache but not part of the process's memory.
/// Writes are collected per thread into regions; a full region stays in
/// memory or is written out in one piece.
pub struct Spool {
    dir: PathBuf,
    rest: Arc<Rest>,
    file: OnceLock<std::result::Result<File, String>>,
    len: AtomicU64,
    shards: Vec<Mutex<Option<Region>>>,
    /// Full regions in memory, by start.
    kept: RwLock<BTreeMap<u64, Vec<u8>>>,
    kept_bytes: AtomicU64,
    /// Regions in the file, by start, with their lengths.
    written: RwLock<BTreeMap<u64, usize>>,
    cache: Mutex<Vec<(u64, Arc<Vec<u8>>)>>,
}

/// A region being filled.
struct Region {
    start: u64,
    buf: Vec<u8>,
}

impl Spool {
    pub fn new(dir: &Path, rest: &Arc<Rest>) -> Arc<Spool> {
        Arc::new(Spool {
            dir: dir.to_path_buf(),
            rest: rest.clone(),
            file: OnceLock::new(),
            len: AtomicU64::new(0),
            shards: (0..SPOOL_SHARDS).map(|_| Mutex::new(None)).collect(),
            kept: RwLock::new(BTreeMap::new()),
            kept_bytes: AtomicU64::new(0),
            written: RwLock::new(BTreeMap::new()),
            cache: Mutex::new(Vec::new()),
        })
    }

    fn file(&self) -> crate::Result<&File> {
        self.file
            .get_or_init(|| create_unlinked(&self.dir, "spool"))
            .as_ref()
            .map_err(|m| Error::new(EXIT_OPEN, m.clone()))
    }

    /// Keeps a full region in memory if there is room, or writes it out.
    fn retire(&self, start: u64, buf: Vec<u8>) -> crate::Result<()> {
        if self.rest.take(buf.capacity()) {
            self.kept_bytes.fetch_add(buf.capacity() as u64, Relaxed);
            self.kept.write().unwrap().insert(start, buf);
            return Ok(());
        }
        self.file()?
            .write_all_at(&buf, start)
            .map_err(|e| Error::new(EXIT_WRITE, format!("{}: {}", self.dir.display(), e)))?;
        self.written.write().unwrap().insert(start, buf.len());
        Ok(())
    }

    /// Stores `data`, returning where it is.
    pub fn store(self: &Arc<Self>, data: &[u8]) -> crate::Result<Stored> {
        self.store_parts(&[data])
    }

    /// Stores the parts one after the other, returning where they are.
    pub fn store_parts(self: &Arc<Self>, parts: &[&[u8]]) -> crate::Result<Stored> {
        let len: usize = parts.iter().map(|p| p.len()).sum();
        if len >= REGION / 4 {
            let at = self.len.fetch_add(len as u64, Relaxed);
            if self.rest.take(len) {
                self.kept_bytes.fetch_add(len as u64, Relaxed);
                self.kept.write().unwrap().insert(at, parts.concat());
            } else {
                let mut off = at;
                for p in parts {
                    self.file()?.write_all_at(p, off).map_err(|e| {
                        Error::new(EXIT_WRITE, format!("{}: {}", self.dir.display(), e))
                    })?;
                    off += p.len() as u64;
                }
                self.written.write().unwrap().insert(at, len);
            }
            return Ok(Stored::Spooled {
                spool: self.clone(),
                shard: NO_SHARD,
                at,
                len,
            });
        }
        let shard = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            std::thread::current().id().hash(&mut h);
            (h.finish() % SPOOL_SHARDS as u64) as usize
        };
        let mut region = self.shards[shard].lock().unwrap();
        if region.as_ref().is_none_or(|r| r.buf.len() + len > REGION) {
            if let Some(r) = region.take() {
                self.retire(r.start, r.buf)?;
            }
            *region = Some(Region {
                start: self.len.fetch_add(REGION as u64, Relaxed),
                buf: Vec::with_capacity(REGION),
            });
        }
        let r = region.as_mut().unwrap();
        let at = r.start + r.buf.len() as u64;
        for p in parts {
            r.buf.extend_from_slice(p);
        }
        Ok(Stored::Spooled {
            spool: self.clone(),
            shard: shard as u32,
            at,
            len,
        })
    }

    /// Reads `out.len()` bytes from `at`. With `sequential`, a region read
    /// from the file is kept for the reads that follow, for a reader that
    /// goes through the data in about the order it was written.
    fn read(&self, shard: u32, at: u64, out: &mut [u8], sequential: bool) -> crate::Result<()> {
        let n = out.len() as u64;
        let within = |start: u64, len: usize| at >= start && at + n <= start + len as u64;
        // a region leaves its shard only after it is kept or written out
        if let Some(region) = self.shards.get(shard as usize) {
            let region = region.lock().unwrap();
            if let Some(r) = region.as_ref()
                && within(r.start, r.buf.len())
            {
                let from = (at - r.start) as usize;
                out.copy_from_slice(&r.buf[from..from + out.len()]);
                return Ok(());
            }
        }
        {
            let kept = self.kept.read().unwrap();
            if let Some((&start, buf)) = kept.range(..=at).next_back()
                && within(start, buf.len())
            {
                let from = (at - start) as usize;
                out.copy_from_slice(&buf[from..from + out.len()]);
                return Ok(());
            }
        }
        let region = self
            .written
            .read()
            .unwrap()
            .range(..=at)
            .next_back()
            .map(|(&s, &l)| (s, l));
        let file = self.file()?;
        let err =
            |e: std::io::Error| Error::new(EXIT_READ, format!("{}: {}", self.dir.display(), e));
        match region {
            Some((start, len)) if sequential && len <= REGION && within(start, len) => {
                let cached = self
                    .cache
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(s, _)| *s == start)
                    .map(|(_, b)| b.clone());
                let buf = match cached {
                    Some(b) => b,
                    None => {
                        let mut b = vec![0u8; len];
                        file.read_exact_at(&mut b, start).map_err(err)?;
                        let b = Arc::new(b);
                        let mut cache = self.cache.lock().unwrap();
                        if cache.len() >= CACHED {
                            cache.remove(0);
                        }
                        cache.push((start, b.clone()));
                        b
                    }
                };
                let from = (at - start) as usize;
                out.copy_from_slice(&buf[from..from + out.len()]);
                Ok(())
            }
            _ => file.read_exact_at(out, at).map_err(err),
        }
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        self.rest.give(self.kept_bytes.load(Relaxed) as usize);
    }
}

/// Bytes in memory, or stored in a spool.
pub enum Stored {
    Mem(Buf<u8>),
    Spooled {
        spool: Arc<Spool>,
        /// The buffer it went through, if any.
        shard: u32,
        at: u64,
        len: usize,
    },
}

/// Where some stored bytes are in a spool that their holder knows:
/// `Stored::Spooled` without the spool, for records kept by the million.
#[derive(Clone, Copy, Debug, Default)]
pub struct Place {
    pub at: u64,
    pub len: u64,
    pub shard: u32,
}

impl Place {
    /// The bytes from `skip` on, as stored bytes of `spool`.
    pub fn stored(&self, spool: &Arc<Spool>, skip: u64) -> Stored {
        debug_assert!(skip <= self.len);
        Stored::Spooled {
            spool: spool.clone(),
            shard: self.shard,
            at: self.at + skip,
            len: (self.len - skip) as usize,
        }
    }

    /// Reads `out.len()` bytes from the start.
    pub fn read(&self, spool: &Spool, out: &mut [u8]) -> crate::Result<()> {
        debug_assert!(out.len() as u64 <= self.len);
        spool.read(self.shard, self.at, out, false)
    }

    /// The shard in a byte, for records that keep places by the million.
    pub fn shard_byte(&self) -> u8 {
        if self.shard == NO_SHARD {
            u8::MAX
        } else {
            debug_assert!((self.shard as usize) < SPOOL_SHARDS);
            self.shard as u8
        }
    }

    /// The place with the shard from `shard_byte`.
    pub fn with_shard_byte(at: u64, len: u64, shard: u8) -> Place {
        Place {
            at,
            len,
            shard: if shard == u8::MAX {
                NO_SHARD
            } else {
                shard as u32
            },
        }
    }
}

impl Stored {
    /// Where the bytes are, if they are in `spool`.
    pub fn place_in(&self, spool: &Arc<Spool>) -> Option<Place> {
        match self {
            Stored::Spooled {
                spool: s,
                shard,
                at,
                len,
            } if Arc::ptr_eq(s, spool) => Some(Place {
                at: *at,
                len: *len as u64,
                shard: *shard,
            }),
            _ => None,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Stored::Mem(v) => v.len(),
            Stored::Spooled { len, .. } => *len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn load(&self) -> crate::Result<Loaded<'_>> {
        self.load1(false)
    }

    /// `load`, for a reader that goes through a spool in about the order it
    /// was written.
    pub fn load_sequential(&self) -> crate::Result<Loaded<'_>> {
        self.load1(true)
    }

    fn load1(&self, sequential: bool) -> crate::Result<Loaded<'_>> {
        match self {
            Stored::Mem(v) => Ok(Loaded::Borrowed(v)),
            Stored::Spooled {
                spool,
                shard,
                at,
                len,
            } => {
                let mut out = Buf::with_capacity(*len);
                out.resize(*len, 0u8);
                spool.read(*shard, *at, &mut out, sequential)?;
                Ok(Loaded::Owned(out))
            }
        }
    }
}

/// Stored bytes at hand.
pub enum Loaded<'a> {
    Borrowed(&'a [u8]),
    Owned(Buf<u8>),
}

impl std::ops::Deref for Loaded<'_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Loaded::Borrowed(b) => b,
            Loaded::Owned(b) => b,
        }
    }
}

/// Creates a file in `dir` and unlinks it at once, as tippecanoe does with
/// its temporary files, so that it goes away however the process ends.
pub fn create_unlinked(dir: &Path, what: &str) -> std::result::Result<File, String> {
    for attempt in 0..100u32 {
        let name = dir.join(format!(
            "manby-{}-{}-{}-{}",
            what,
            std::process::id(),
            attempt,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&name)
        {
            Ok(f) => {
                let _ = std::fs::remove_file(&name);
                return Ok(f);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{}: {}", name.display(), e)),
        }
    }
    Err(format!("{}: cannot create a temporary file", dir.display()))
}
