//! Port of memfile.cpp: a temporary file that is kept in memory until it gets
//! too big, and then appended to on disk.

use std::fs::File;
use std::io::{BufWriter, Write};

use crate::cstd::{errno, strerror};
use crate::errors::{EXIT_IMPOSSIBLE, EXIT_OPEN, EXIT_WRITE};

#[allow(dead_code)]
const INCREMENT: usize = 131072;
const INITIAL: i64 = 256;

pub struct Memfile {
    pub fd: i32,
    pub map: Vec<u8>,
    pub tree: u64,
    pub fp: Option<BufWriter<File>>,
    pub off: usize,
}

pub fn memfile_open(fd: i32) -> Option<Box<Memfile>> {
    if crate::platform::ftruncate(fd, INITIAL) != 0 {
        return None;
    }

    Some(Box::new(Memfile {
        fd,
        map: Vec::new(),
        tree: 0,
        fp: None,
        off: 0,
    }))
}

pub fn memfile_close(file: Box<Memfile>) -> i32 {
    // If it isn't full yet, flush out the string to the file now.
    // If it is full, close out the buffered file writer.

    let mut file = file;
    match file.fp.take() {
        None => {
            let w = crate::platform::write(file.fd, &file.map);
            if w != file.map.len() as isize {
                return -1;
            }

            // SAFETY: closes the descriptor the memfile owns, once.
            if file.fd >= 0 && unsafe { libc::close(file.fd) } != 0 {
                return -1;
            }
        }
        Some(fp) => {
            // The File closes the descriptor when it is dropped.
            if fp.into_inner().is_err() {
                return -1;
            }
            crate::platform::close_shared_fd(file.fd);
        }
    }

    0
}

pub fn memfile_write(file: &mut Memfile, s: &[u8], in_memory: &mut bool) -> i64 {
    // If it is full, append to the file.
    // If it is not full yet, append to the string in memory.

    *in_memory = false;
    if let Some(fp) = file.fp.as_mut() {
        if fp.write_all(s).is_err() {
            return 0;
        }
        file.off += s.len();
    } else {
        file.map.extend_from_slice(s);
        file.off += s.len();
        *in_memory = true;
    }

    s.len() as i64
}

pub fn memfile_full(file: &mut Memfile) {
    // The file is full. Write out a copy of whatever has accumulated in memory
    // to the file, and switch to appending to the file. Existing references
    // into the memory still work.

    if file.fp.is_some() {
        eprintln!("memfile marked full twice");
        std::process::exit(EXIT_IMPOSSIBLE);
    }

    if file.fd < 0 {
        eprintln!("fdopen memfile: {}", strerror(libc::EBADF));
        std::process::exit(EXIT_OPEN);
    }

    // SAFETY: the memfile owns the descriptor, and from here on the File does.
    let mut fp = BufWriter::new(unsafe { crate::platform::file_sharing_fd(file.fd) });

    if fp.write_all(&file.map).is_err() {
        eprintln!("memfile write: {}", strerror(errno()));
        std::process::exit(EXIT_WRITE);
    }

    file.fp = Some(fp);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpfd() -> i32 {
        let mut name = format!("{}/memfile-test.XXXXXX", crate::platform::default_tmpdir());
        let fd = crate::tippecanoe_main::mkstemp_cloexec(&mut name);
        assert!(fd >= 0);
        std::fs::remove_file(&name).unwrap();
        fd
    }

    #[test]
    fn memory_then_file() {
        let fd = tmpfd();
        let mut mf = memfile_open(fd).unwrap();
        let mut in_memory = false;

        assert_eq!(memfile_write(&mut mf, b"hello", &mut in_memory), 5);
        assert!(in_memory);
        assert_eq!(mf.off, 5);

        memfile_full(&mut mf);
        assert_eq!(memfile_write(&mut mf, b" world", &mut in_memory), 6);
        assert!(!in_memory);
        assert_eq!(mf.off, 11);
        assert_eq!(mf.map, b"hello");

        mf.fp.as_mut().unwrap().flush().unwrap();
        let mut buf = vec![0u8; 11];
        let n = crate::platform::pread(mf.fd, &mut buf, 0);
        assert_eq!(n, 11);
        assert_eq!(&buf, b"hello world");

        assert_eq!(memfile_close(mf), 0);
    }
}
