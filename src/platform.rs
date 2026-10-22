//! Port of platform.cpp.

use crate::cstd::perror;
use crate::errors::{EXIT_MEMORY, EXIT_PTHREAD};

pub fn get_num_avail_cpus() -> i64 {
    // SAFETY: sysconf has no preconditions.
    unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) as i64 }
}

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

#[cfg(not(target_os = "macos"))]
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

pub const fn get_null_device() -> &'static str {
    "/dev/null"
}
