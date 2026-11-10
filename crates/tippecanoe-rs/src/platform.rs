//! Port of platform.cpp.

use std::fs::File;
use std::mem::ManuallyDrop;

use crate::cstd::perror;
use crate::errors::EXIT_MEMORY;
#[cfg(unix)]
use crate::errors::EXIT_PTHREAD;

#[cfg(unix)]
pub fn get_num_avail_cpus() -> i64 {
    // SAFETY: sysconf has no preconditions.
    unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) as i64 }
}

#[cfg(unix)]
pub fn get_page_size() -> i64 {
    // SAFETY: sysconf has no preconditions.
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as i64 }
}

#[cfg(target_os = "macos")]
pub fn calc_memsize() -> usize {
    let mut hw_memsize: i64 = 0;
    let mut len: libc::size_t = std::mem::size_of::<i64>();
    let name = c"hw.memsize";
    // SAFETY: the output buffer is an i64 and len holds its size.
    let ret = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            &mut hw_memsize as *mut i64 as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret < 0 {
        perror("sysctl hw.memsize");
        std::process::exit(EXIT_MEMORY);
    }
    hw_memsize as usize
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn calc_memsize() -> usize {
    // SAFETY: sysconf has no preconditions.
    let pagesize = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as i64;
    // SAFETY: sysconf has no preconditions.
    let pages = unsafe { libc::sysconf(libc::_SC_PHYS_PAGES) } as i64;
    if pages < 0 || pagesize < 0 {
        perror("sysconf _SC_PAGESIZE or _SC_PHYS_PAGES");
        std::process::exit(EXIT_MEMORY);
    }
    (pages * pagesize) as usize
}

#[cfg(unix)]
pub fn get_max_open_files() -> usize {
    let mut rl = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: rl is a valid rlimit to fill in.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) } != 0 {
        perror("getrlimit");
        std::process::exit(EXIT_PTHREAD);
    }
    rl.rlim_cur as usize
}

#[cfg(unix)]
pub const fn get_null_device() -> &'static str {
    "/dev/null"
}

// Windows has none of the calls above, and its C runtime's descriptors lack
// what tippecanoe asks of them: temporary files are removed by name while
// still open, and filters run as forked children. What follows gives the
// rest of the port the same calls on both, unchanged on Unix.

#[cfg(windows)]
pub fn get_num_avail_cpus() -> i64 {
    std::thread::available_parallelism().map_or(1, |n| n.get() as i64)
}

#[cfg(windows)]
fn system_info() -> windows_sys::Win32::System::SystemInformation::SYSTEM_INFO {
    // SAFETY: GetSystemInfo fills in the zeroed struct and cannot fail.
    unsafe {
        let mut si = std::mem::zeroed();
        windows_sys::Win32::System::SystemInformation::GetSystemInfo(&mut si);
        si
    }
}

#[cfg(windows)]
pub fn get_page_size() -> i64 {
    system_info().dwPageSize as i64
}

#[cfg(windows)]
pub fn calc_memsize() -> usize {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: the struct is plain data, given its own length as the call requires.
    let mut ms: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    ms.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    // SAFETY: ms is a valid MEMORYSTATUSEX with dwLength set.
    if unsafe { GlobalMemoryStatusEx(&mut ms) } == 0 {
        perror("GlobalMemoryStatusEx");
        std::process::exit(EXIT_MEMORY);
    }
    ms.ullTotalPhys as usize
}

/// The C runtime's limit on open descriptors.
#[cfg(windows)]
pub fn get_max_open_files() -> usize {
    8192
}

#[cfg(windows)]
pub const fn get_null_device() -> &'static str {
    "NUL"
}

/// Whether `c` separates the components of a path: `/`, and on Windows `\\`
/// too.
pub fn is_path_separator(c: u8) -> bool {
    c == b'/' || (cfg!(windows) && c == b'\\')
}

/// The last component of a path, as a program's name: on Windows without
/// `.exe`.
pub fn program_name(path: &str) -> &str {
    let name = path
        .rsplit(|c: char| c.is_ascii() && is_path_separator(c as u8))
        .next()
        .unwrap_or(path);
    if cfg!(windows) {
        name.strip_suffix(".exe").unwrap_or(name)
    } else {
        name
    }
}

/// Where temporary files go when nothing says otherwise.
pub fn default_tmpdir() -> String {
    if cfg!(windows) {
        std::env::temp_dir().to_string_lossy().into_owned()
    } else {
        "/tmp".to_string()
    }
}

/// A process id, as `fork` gives on Unix. On Windows, a key for the child
/// process that `wait_child` waits for.
#[cfg(unix)]
pub type Pid = libc::pid_t;
#[cfg(windows)]
pub type Pid = i32;

/// The flag for a descriptor that a child process does not inherit.
#[cfg(unix)]
pub const O_CLOEXEC: i32 = libc::O_CLOEXEC;
#[cfg(windows)]
pub const O_CLOEXEC: i32 = libc::O_NOINHERIT;

/// `open(path, flags, mode)`, returning the descriptor or -1 with errno set.
#[cfg(unix)]
pub fn open(path: &str, flags: i32, mode: i32) -> i32 {
    match std::ffi::CString::new(path) {
        // SAFETY: open of a NUL-terminated path.
        Ok(c) => unsafe { libc::open(c.as_ptr(), flags, mode as libc::c_uint) },
        Err(_) => {
            crate::cstd::set_errno(libc::ENOENT);
            -1
        }
    }
}

/// `open(path, flags, mode)`. The file is opened in binary mode and shared
/// for deletion, so that it can be removed by name while open, as on Unix.
#[cfg(windows)]
pub fn open(path: &str, flags: i32, _mode: i32) -> i32 {
    use std::os::windows::fs::OpenOptionsExt;
    let mut o = std::fs::OpenOptions::new();
    match flags & 3 {
        libc::O_WRONLY => o.write(true),
        libc::O_RDWR => o.read(true).write(true),
        _ => o.read(true),
    };
    if flags & libc::O_APPEND != 0 {
        o.append(true);
    }
    if flags & libc::O_CREAT != 0 {
        if flags & libc::O_EXCL != 0 {
            o.create_new(true);
        } else {
            o.create(true);
        }
    }
    if flags & libc::O_TRUNC != 0 {
        o.truncate(true);
    }
    // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
    o.share_mode(7);
    match o.open(path) {
        Ok(f) => file_into_fd(f),
        Err(e) => {
            crate::cstd::set_errno(io_errno(&e));
            -1
        }
    }
}

/// The errno for an error from the standard library.
#[cfg(windows)]
pub fn io_errno(e: &std::io::Error) -> i32 {
    use std::io::ErrorKind::*;
    match e.kind() {
        NotFound => libc::ENOENT,
        PermissionDenied => libc::EACCES,
        AlreadyExists => libc::EEXIST,
        InvalidInput => libc::EINVAL,
        _ => libc::EIO,
    }
}

/// libc `mkstemp`. `name` is a template ending in XXXXXX, which is replaced
/// with the name of the file created. Returns the file descriptor, or -1.
#[cfg(unix)]
pub fn mkstemp(name: &mut String) -> i32 {
    let mut buf = name.clone().into_bytes();
    buf.push(0);
    // SAFETY: buf is a writable NUL-terminated template.
    let fd = unsafe { libc::mkstemp(buf.as_mut_ptr() as *mut libc::c_char) };
    buf.pop();
    *name = String::from_utf8_lossy(&buf).into_owned();
    fd
}

/// libc `mkstemp`, with the file opened as `open` opens it, so that it can be
/// unlinked while open.
#[cfg(windows)]
pub fn mkstemp(name: &mut String) -> i32 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let Some(stem) = name.strip_suffix("XXXXXX").map(str::to_string) else {
        crate::cstd::set_errno(libc::EINVAL);
        return -1;
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    for _ in 0..100 {
        let mut v = nanos
            ^ (std::process::id() as u64).rotate_left(32)
            ^ SEQ
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15);
        v = v.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        let mut t = stem.clone();
        for _ in 0..6 {
            t.push(CHARS[(v % CHARS.len() as u64) as usize] as char);
            v /= CHARS.len() as u64;
        }
        let fd = open(&t, libc::O_RDWR | libc::O_CREAT | libc::O_EXCL, 0o600);
        if fd >= 0 || crate::cstd::errno() != libc::EEXIST {
            *name = t;
            return fd;
        }
    }
    -1
}

/// `unlink(path)`.
pub fn unlink(path: &str) -> i32 {
    match std::ffi::CString::new(path) {
        // SAFETY: unlink of a NUL-terminated path.
        Ok(c) => unsafe { libc::unlink(c.as_ptr()) },
        Err(_) => -1,
    }
}

/// `lseek(fd, off, whence)`, with 64-bit offsets on Windows too.
#[cfg(unix)]
pub fn lseek(fd: i32, off: i64, whence: i32) -> i64 {
    // SAFETY: lseek has no memory preconditions.
    unsafe { libc::lseek(fd, off as libc::off_t, whence) as i64 }
}

#[cfg(windows)]
pub fn lseek(fd: i32, off: i64, whence: i32) -> i64 {
    // SAFETY: lseek has no memory preconditions.
    unsafe { libc::lseek64(fd, off, whence) }
}

/// `pread(fd, buf, off)`.
pub fn pread(fd: i32, buf: &mut [u8], off: u64) -> isize {
    match borrow_fd(fd).read_at(buf, off) {
        Ok(n) => n as isize,
        Err(e) => {
            crate::cstd::set_errno(errno_of(&e));
            -1
        }
    }
}

#[cfg(unix)]
fn errno_of(e: &std::io::Error) -> i32 {
    e.raw_os_error().unwrap_or(libc::EIO)
}

#[cfg(windows)]
fn errno_of(e: &std::io::Error) -> i32 {
    io_errno(e)
}

/// A `File` that takes over the descriptor `fd`.
///
/// # Safety
///
/// `fd` is open, and nothing else closes it.
#[cfg(unix)]
pub unsafe fn file_from_fd(fd: i32) -> File {
    use std::os::fd::FromRawFd;
    // SAFETY: as the caller promises.
    unsafe { File::from_raw_fd(fd) }
}

/// A `File` that takes over the descriptor `fd`: a copy of its handle, after
/// which the descriptor is closed.
///
/// # Safety
///
/// `fd` is open, and nothing else closes it.
#[cfg(windows)]
pub unsafe fn file_from_fd(fd: i32) -> File {
    let f = copy_fd_file(fd);
    // SAFETY: the caller owns fd, and nothing uses it after this.
    unsafe { libc::close(fd) };
    f
}

/// A `File` with a copy of the handle of `fd`, which stays open.
#[cfg(windows)]
pub fn copy_fd_file(fd: i32) -> File {
    match borrow_fd(fd).try_clone() {
        Ok(f) => f,
        Err(e) => {
            crate::cstd::set_errno(io_errno(&e));
            perror("duplicate file handle");
            std::process::exit(crate::errors::EXIT_OPEN);
        }
    }
}

/// A `File` for `fd`, whose number stays usable while the `File` lives. On
/// Unix the `File` takes over the descriptor, as `fdopen` does. On Windows it
/// holds a copy of the handle, and `close_shared_fd` closes the descriptor
/// once the caller is done with both.
///
/// # Safety
///
/// `fd` is open, and nothing else closes it.
pub unsafe fn file_sharing_fd(fd: i32) -> File {
    #[cfg(unix)]
    // SAFETY: as the caller promises.
    return unsafe { file_from_fd(fd) };
    #[cfg(windows)]
    return copy_fd_file(fd);
}

/// Closes a descriptor given to `file_sharing_fd`, on Windows. On Unix the
/// `File` closed it.
pub fn close_shared_fd(fd: i32) {
    // SAFETY: the caller is done with the descriptor.
    #[cfg(windows)]
    unsafe {
        libc::close(fd);
    }
    #[cfg(unix)]
    let _ = fd;
}

/// The descriptor of `f`, which the caller then owns.
#[cfg(unix)]
pub fn file_into_fd(f: File) -> i32 {
    use std::os::fd::IntoRawFd;
    f.into_raw_fd()
}

#[cfg(windows)]
pub fn file_into_fd(f: File) -> i32 {
    use std::os::windows::io::IntoRawHandle;
    let h = f.into_raw_handle();
    // SAFETY: the descriptor takes over the handle, which nothing else owns.
    let fd =
        unsafe { libc::open_osfhandle(h as libc::intptr_t, libc::O_BINARY | libc::O_NOINHERIT) };
    if fd < 0 {
        use std::os::windows::io::FromRawHandle;
        // SAFETY: the handle is still this function's to close.
        drop(unsafe { File::from_raw_handle(h) });
    }
    fd
}

/// A `File` for a descriptor that something else owns and will close.
#[cfg(unix)]
pub fn borrow_fd(fd: i32) -> ManuallyDrop<File> {
    use std::os::fd::FromRawFd;
    // SAFETY: the File is never dropped, so it never closes the descriptor.
    ManuallyDrop::new(unsafe { File::from_raw_fd(fd) })
}

#[cfg(windows)]
pub fn borrow_fd(fd: i32) -> ManuallyDrop<File> {
    use std::os::windows::io::FromRawHandle;
    // SAFETY: get_osfhandle only looks the descriptor up.
    let h = unsafe { libc::get_osfhandle(fd) };
    // SAFETY: the File is never dropped, so it never closes the handle.
    ManuallyDrop::new(unsafe { File::from_raw_handle(h as _) })
}

/// Reads and writes at an offset, as `pread` and `pwrite` do. Windows moves
/// the file position as it reads or writes, so there the position is put back
/// afterwards, for callers that go on appending. Calls on the same file from
/// several threads at once may then leave it anywhere.
pub trait FileAt {
    fn read_at(&self, buf: &mut [u8], off: u64) -> std::io::Result<usize>;
    fn write_at(&self, buf: &[u8], off: u64) -> std::io::Result<usize>;
    fn read_exact_at(&self, buf: &mut [u8], off: u64) -> std::io::Result<()>;
    fn write_all_at(&self, buf: &[u8], off: u64) -> std::io::Result<()>;
}

#[cfg(unix)]
impl FileAt for File {
    fn read_at(&self, buf: &mut [u8], off: u64) -> std::io::Result<usize> {
        std::os::unix::fs::FileExt::read_at(self, buf, off)
    }
    fn write_at(&self, buf: &[u8], off: u64) -> std::io::Result<usize> {
        std::os::unix::fs::FileExt::write_at(self, buf, off)
    }
    fn read_exact_at(&self, buf: &mut [u8], off: u64) -> std::io::Result<()> {
        std::os::unix::fs::FileExt::read_exact_at(self, buf, off)
    }
    fn write_all_at(&self, buf: &[u8], off: u64) -> std::io::Result<()> {
        std::os::unix::fs::FileExt::write_all_at(self, buf, off)
    }
}

#[cfg(windows)]
impl FileAt for File {
    fn read_at(&self, buf: &mut [u8], off: u64) -> std::io::Result<usize> {
        keeping_position(self, |f| {
            std::os::windows::fs::FileExt::seek_read(f, buf, off)
        })
    }
    fn write_at(&self, buf: &[u8], off: u64) -> std::io::Result<usize> {
        keeping_position(self, |f| {
            std::os::windows::fs::FileExt::seek_write(f, buf, off)
        })
    }
    fn read_exact_at(&self, mut buf: &mut [u8], mut off: u64) -> std::io::Result<()> {
        keeping_position(self, move |f| {
            while !buf.is_empty() {
                match std::os::windows::fs::FileExt::seek_read(f, buf, off) {
                    Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
                    Ok(n) => {
                        buf = &mut buf[n..];
                        off += n as u64;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        })
    }
    fn write_all_at(&self, mut buf: &[u8], mut off: u64) -> std::io::Result<()> {
        keeping_position(self, move |f| {
            while !buf.is_empty() {
                match std::os::windows::fs::FileExt::seek_write(f, buf, off) {
                    Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                    Ok(n) => {
                        buf = &buf[n..];
                        off += n as u64;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        })
    }
}

#[cfg(windows)]
fn keeping_position<T>(
    f: &File,
    op: impl FnOnce(&File) -> std::io::Result<T>,
) -> std::io::Result<T> {
    use std::io::{Seek, SeekFrom};
    let mut h = f;
    let pos = h.stream_position()?;
    let ret = op(f);
    h.seek(SeekFrom::Start(pos))?;
    ret
}

/// Whether `st` is a directory, as `st_mode & S_IFDIR` tests it.
#[cfg(unix)]
pub fn mode_is_dir(st: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    (st.mode() & libc::S_IFDIR as u32) != 0
}

#[cfg(windows)]
pub fn mode_is_dir(st: &std::fs::Metadata) -> bool {
    st.is_dir()
}

/// Makes writes to a closed pipe end the program quietly, as `SIGPIPE` does
/// by default. Windows has no such signal.
pub fn sigpipe_default() {
    // SAFETY: setting a signal to its default action has no preconditions.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

/// Makes writes to a closed pipe fail with `EPIPE` instead.
pub fn sigpipe_ignore() {
    // SAFETY: ignoring SIGPIPE has no preconditions.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

/// Seconds since 1970, as `gettimeofday` gives them, or `None`.
#[cfg(unix)]
pub fn time_of_day() -> Option<f64> {
    let mut tv = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    // SAFETY: tv is a valid timeval to fill in.
    if unsafe { libc::gettimeofday(&mut tv, std::ptr::null_mut()) } != 0 {
        None
    } else {
        Some(tv.tv_sec as f64 + tv.tv_usec as f64 / 1000000.0)
    }
}

#[cfg(windows)]
pub fn time_of_day() -> Option<f64> {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some(d.as_secs() as f64 + d.subsec_micros() as f64 / 1000000.0)
}

/// The free space on the disk that holds `fd`, as `fstatfs` gives it, or
/// `Err(errno)`.
#[cfg(unix)]
pub fn disk_free(fd: i32) -> Result<i64, i32> {
    // SAFETY: fsstat is a valid statfs buffer to fill in.
    let mut fsstat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: fstatfs of a descriptor into a valid buffer.
    if unsafe { libc::fstatfs(fd, &mut fsstat) } != 0 {
        Err(crate::cstd::errno())
    } else {
        Ok((fsstat.f_bsize as i64 as u64).wrapping_mul(fsstat.f_bavail as u64) as i64)
    }
}

#[cfg(windows)]
pub fn disk_free(fd: i32) -> Result<i64, i32> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetFinalPathNameByHandleW};
    let f = borrow_fd(fd);
    let mut path = vec![0u16; 32768];
    // SAFETY: path has room for the length given.
    let n = unsafe {
        GetFinalPathNameByHandleW(f.as_raw_handle(), path.as_mut_ptr(), path.len() as u32, 0)
    } as usize;
    if n == 0 || n >= path.len() {
        return Err(libc::EIO);
    }
    // The directory that holds the file.
    let end = path[..n]
        .iter()
        .rposition(|&c| c == b'\\' as u16)
        .unwrap_or(n);
    path.truncate(end + 1);
    path.push(0);
    let mut avail: u64 = 0;
    // SAFETY: path is NUL-terminated and avail a valid place for the result.
    if unsafe {
        GetDiskFreeSpaceExW(
            path.as_ptr(),
            &mut avail,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(libc::EIO);
    }
    Ok(avail as i64)
}

/// Advice for a mapping, as for `madvise`. Windows takes none.
#[cfg(unix)]
pub use memmap2::Advice;

#[cfg(windows)]
#[derive(Clone, Copy, Debug)]
pub enum Advice {
    Normal,
    Random,
    Sequential,
    WillNeed,
}

/// `madvise`, whose result the C++ ignores.
pub fn madvise(map: &memmap2::Mmap, advice: Advice) {
    #[cfg(unix)]
    let _ = map.advise(advice);
    #[cfg(windows)]
    let _ = (map, advice);
}

/// `madvise` of a writable mapping.
pub fn madvise_mut(map: &memmap2::MmapMut, advice: Advice) {
    #[cfg(unix)]
    let _ = map.advise(advice);
    #[cfg(windows)]
    let _ = (map, advice);
}

/// `ftruncate(fd, len)`.
#[cfg(unix)]
pub fn ftruncate(fd: i32, len: i64) -> i32 {
    // SAFETY: ftruncate has no memory preconditions.
    unsafe { libc::ftruncate(fd, len as libc::off_t) }
}

#[cfg(windows)]
pub fn ftruncate(fd: i32, len: i64) -> i32 {
    match borrow_fd(fd).set_len(len as u64) {
        Ok(()) => 0,
        Err(e) => {
            crate::cstd::set_errno(io_errno(&e));
            -1
        }
    }
}

/// `write(fd, buf)`.
pub fn write(fd: i32, buf: &[u8]) -> isize {
    // SAFETY: writes the bytes of `buf`, which stays alive for the call.
    unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len() as _) as isize }
}
