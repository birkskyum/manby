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

use std::borrow::Cow;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};

use tippecanoe::errors::{EXIT_OPEN, EXIT_READ, EXIT_WRITE};

use crate::Error;

/// How many bytes a spool collects in memory before one write.
const REGION: usize = 256 << 10;
/// Writers go to one of this many buffers, by thread.
const SPOOL_SHARDS: usize = 8;
const NO_SHARD: u32 = u32::MAX;

/// A part of the file that one buffer fills before it is written.
struct Region {
    start: u64,
    buf: Vec<u8>,
}

/// An unlinked temporary file for data at rest: written once, read back
/// later, and gone when the last reference is dropped. Its pages stay in
/// the page cache while there is room, so reading it back is cheap, but
/// they are not part of the process's memory. Small writes are collected
/// per thread and written a region at a time.
pub struct Spool {
    dir: PathBuf,
    file: OnceLock<std::result::Result<File, String>>,
    len: AtomicU64,
    shards: Vec<Mutex<Option<Region>>>,
}

impl Spool {
    pub fn new(dir: &Path) -> Arc<Spool> {
        Arc::new(Spool {
            dir: dir.to_path_buf(),
            file: OnceLock::new(),
            len: AtomicU64::new(0),
            shards: (0..SPOOL_SHARDS).map(|_| Mutex::new(None)).collect(),
        })
    }

    fn file(&self) -> crate::Result<&File> {
        self.file
            .get_or_init(|| create_unlinked(&self.dir, "spool"))
            .as_ref()
            .map_err(|m| Error::new(EXIT_OPEN, m.clone()))
    }

    fn write_at(&self, data: &[u8], at: u64) -> crate::Result<()> {
        self.file()?
            .write_all_at(data, at)
            .map_err(|e| Error::new(EXIT_WRITE, format!("{}: {}", self.dir.display(), e)))
    }

    /// Writes `data`, returning the buffer it went through and where it is.
    fn write(&self, data: &[u8]) -> crate::Result<(u32, u64)> {
        if data.len() >= REGION / 4 {
            let at = self.len.fetch_add(data.len() as u64, Relaxed);
            self.write_at(data, at)?;
            return Ok((NO_SHARD, at));
        }
        let shard = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            std::thread::current().id().hash(&mut h);
            (h.finish() % SPOOL_SHARDS as u64) as u32
        };
        let mut region = self.shards[shard as usize].lock().unwrap();
        if region
            .as_ref()
            .is_none_or(|r| r.buf.len() + data.len() > REGION)
        {
            if let Some(r) = region.take() {
                self.write_at(&r.buf, r.start)?;
            }
            *region = Some(Region {
                start: self.len.fetch_add(REGION as u64, Relaxed),
                buf: Vec::with_capacity(REGION),
            });
        }
        let r = region.as_mut().unwrap();
        let at = r.start + r.buf.len() as u64;
        r.buf.extend_from_slice(data);
        Ok((shard, at))
    }

    /// Reads `out.len()` bytes from `at`.
    fn read(&self, shard: u32, at: u64, out: &mut [u8]) -> crate::Result<()> {
        if shard != NO_SHARD {
            let region = self.shards[shard as usize].lock().unwrap();
            if let Some(r) = region.as_ref()
                && at >= r.start
                && at < r.start + r.buf.len() as u64
            {
                let from = (at - r.start) as usize;
                out.copy_from_slice(&r.buf[from..from + out.len()]);
                return Ok(());
            }
        }
        self.file()?
            .read_exact_at(out, at)
            .map_err(|e| Error::new(EXIT_READ, format!("{}: {}", self.dir.display(), e)))
    }

    /// Moves `data` into the file.
    pub fn store(self: &Arc<Self>, data: &[u8]) -> crate::Result<Stored> {
        let (shard, at) = self.write(data)?;
        Ok(Stored::Spooled {
            spool: self.clone(),
            shard,
            at,
            len: data.len(),
        })
    }
}

/// How many bytes of data at rest may stay in memory; the rest goes to spools.
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

    /// Keeps `data` in memory if there is room, or moves it into `spool`.
    pub fn keep(self: &Arc<Self>, data: Vec<u8>, spool: &Arc<Spool>) -> crate::Result<Stored> {
        if self.take(data.capacity()) {
            Ok(Stored::Kept(data, self.clone()))
        } else {
            spool.store(&data)
        }
    }

    /// Keeps all of `data` in memory if there is room for all of it.
    pub fn keep_all(self: &Arc<Self>, data: &mut [Stored]) -> bool {
        let n: usize = data.iter().map(|d| d.resident()).sum();
        if !self.take(n) {
            return false;
        }
        for d in data.iter_mut() {
            if let Stored::Mem(v) = d {
                *d = Stored::Kept(std::mem::take(v), self.clone());
            }
        }
        true
    }
}

/// Bytes in memory or in a spool.
pub enum Stored {
    Mem(Vec<u8>),
    /// In memory, counted in a `Rest`.
    Kept(Vec<u8>, Arc<Rest>),
    Spooled {
        spool: Arc<Spool>,
        shard: u32,
        at: u64,
        len: usize,
    },
}

impl Stored {
    pub fn len(&self) -> usize {
        match self {
            Stored::Mem(v) | Stored::Kept(v, _) => v.len(),
            Stored::Spooled { len, .. } => *len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The memory the bytes take while at rest.
    pub fn resident(&self) -> usize {
        match self {
            Stored::Mem(v) | Stored::Kept(v, _) => v.capacity(),
            Stored::Spooled { .. } => 0,
        }
    }

    pub fn load(&self) -> crate::Result<Cow<'_, [u8]>> {
        match self {
            Stored::Mem(v) | Stored::Kept(v, _) => Ok(Cow::Borrowed(v)),
            Stored::Spooled {
                spool,
                shard,
                at,
                len,
            } => {
                let mut out = vec![0u8; *len];
                spool.read(*shard, *at, &mut out)?;
                Ok(Cow::Owned(out))
            }
        }
    }
}

impl Drop for Stored {
    fn drop(&mut self) {
        if let Stored::Kept(v, rest) = self {
            rest.used.fetch_sub(v.capacity() as u64, Relaxed);
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
