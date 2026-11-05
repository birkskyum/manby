//! Port of sort.hpp and sort.cpp: an external quicksort of fixed-size records
//! read from several files, for when there are too many to sort in memory.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::os::fd::FromRawFd;

use crate::cstd::{perror, set_errno};
use crate::errors::EXIT_FAILURE;
use crate::tippecanoe_main::mkstemp;

const MAX_MEMORY: usize = 1024 * 1024 * 1024; // 1 GB

/// `fread(element, width, 1, f)`: 1 if a whole element was read, otherwise 0.
fn fread_element(f: &mut dyn Read, element: &mut [u8]) -> usize {
    let mut got = 0;
    while got < element.len() {
        match f.read(&mut element[got..]) {
            Ok(0) => return 0,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return 0,
        }
    }
    1
}

/// `qsort` of the `width`-byte records in `buf`.
fn qsort(buf: &mut Vec<u8>, width: usize, cmp: fn(&[u8], &[u8]) -> i32) {
    let n = buf.len() / width;
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        cmp(
            &buf[a * width..(a + 1) * width],
            &buf[b * width..(b + 1) * width],
        )
        .cmp(&0)
    });

    let mut sorted = Vec::with_capacity(buf.len());
    for i in order {
        sorted.extend_from_slice(&buf[i * width..(i + 1) * width]);
    }
    sorted.extend_from_slice(&buf[n * width..]);
    *buf = sorted;
}

/// A temporary file to partition into, opened "w+b" and already unlinked.
fn sort_tmpfile(name: &str) -> File {
    let mut t = name.to_string();
    let fd = mkstemp(&mut t);
    if let Ok(c) = std::ffi::CString::new(t.clone()) {
        // SAFETY: unlink of a NUL-terminated path.
        unsafe { libc::unlink(c.as_ptr()) };
    }

    if fd < 0 {
        // fdopen(-1) fails this way
        set_errno(libc::EBADF);
        perror(&t);
        std::process::exit(EXIT_FAILURE);
    }

    // SAFETY: mkstemp returned a new descriptor, which the File now owns.
    unsafe { File::from_raw_fd(fd) }
}

/// `fp` was written; rewind it so it can be read.
fn rewind(fp: BufWriter<File>) -> BufReader<File> {
    let mut f = match fp.into_inner() {
        Ok(f) => f,
        Err(e) => e.into_inner().into_parts().0,
    };
    let _ = f.seek(SeekFrom::Start(0));
    BufReader::new(f)
}

pub fn fqsort(
    inputs: &mut [&mut dyn Read],
    width: usize,
    cmp: fn(&[u8], &[u8]) -> i32,
    out: &mut dyn Write,
    mem: usize,
    tmpdir: &str,
) {
    let pivot: Vec<u8>;
    let mut fp1: BufWriter<File>;
    let mut fp2: BufWriter<File>;

    let mut mem = mem;
    if mem > MAX_MEMORY {
        mem = MAX_MEMORY;
    }

    {
        // read some elements into memory to choose a pivot from
        //
        // this is in its own scope so `buf` can go out of scope
        // before trying to do any sub-sorts.

        let mut buf: Vec<u8> = Vec::new();

        let mut read_everything = false;
        for i in 0..inputs.len() {
            if buf.len() > mem {
                break;
            }

            loop {
                let mut element = vec![0u8; width];

                let n = fread_element(inputs[i], &mut element);
                if n == 0 {
                    if i + 1 == inputs.len() {
                        read_everything = true;
                    }
                    break;
                }

                buf.extend_from_slice(&element);

                if buf.len() > mem {
                    break;
                }
            }
        }

        qsort(&mut buf, width, cmp);

        // If that was everything we have to sort, we are done.

        if read_everything {
            let _ = out.write_all(&buf[..buf.len() / width * width]);
            return;
        }

        // Otherwise, choose a pivot from it, make some temporary files,
        // write what we have to those files, and then partition the rest
        // of the input into them.

        // This would be unstable if the pivot is one of several elements
        // that compare equal. Does it matter?

        let pivot_off = width * (buf.len() / width / 2);
        pivot = buf[pivot_off..(pivot_off + width).min(buf.len())].to_vec();

        let t1 = format!("{}/sort1.XXXXXX", tmpdir);
        let t2 = format!("{}/sort2.XXXXXX", tmpdir);

        fp1 = BufWriter::new(sort_tmpfile(&t1));
        fp2 = BufWriter::new(sort_tmpfile(&t2));

        let _ = fp1.write_all(&buf[..pivot_off]);
        let _ = fp2.write_all(&buf[pivot_off..]);
    }

    // read the remaining input into the temporary files

    for input in inputs.iter_mut() {
        loop {
            let mut element = vec![0u8; width];

            let n = fread_element(*input, &mut element);
            if n == 0 {
                break;
            }

            if cmp(&element, &pivot) < 0 {
                let _ = fp1.write_all(&element);
            } else {
                let _ = fp2.write_all(&element);
            }
        }
    }

    // Now sort the sub-ranges into the output.

    let mut r1 = rewind(fp1);
    let mut r2 = rewind(fp2);

    {
        let mut v1: Vec<&mut dyn Read> = vec![&mut r1];
        fqsort(&mut v1, width, cmp, out, mem, tmpdir);
    }
    drop(r1);

    {
        let mut v2: Vec<&mut dyn Read> = vec![&mut r2];
        fqsort(&mut v2, width, cmp, out, mem, tmpdir);
    }
    drop(r2);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intcmp(v1: &[u8], v2: &[u8]) -> i32 {
        let a = i32::from_ne_bytes(v1.try_into().unwrap());
        let b = i32::from_ne_bytes(v2.try_into().unwrap());
        a.wrapping_sub(b)
    }

    fn tmpfile(name: &str) -> File {
        sort_tmpfile(name)
    }

    // A small deterministic generator, standing in for rand()
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 33) as u32
        }
    }

    #[test]
    fn external_quicksort() {
        let mut rng = Lcg(42);
        let mut inputs: Vec<BufReader<File>> = Vec::new();

        let mut written = 0;
        for _ in 0..5 {
            let mut f = BufWriter::new(tmpfile("/tmp/in.XXXXXXX"));
            let iterations = 2000 + rng.next() % 200;
            for _ in 0..iterations {
                // rand() is never negative
                let n = (rng.next() & 0x7FFF_FFFF) as i32;
                f.write_all(&n.to_ne_bytes()).unwrap();
                written += 1;
            }
            inputs.push(rewind(f));
        }

        let mut out = BufWriter::new(tmpfile("/tmp/out.XXXXXX"));
        {
            let mut refs: Vec<&mut dyn Read> =
                inputs.iter_mut().map(|r| r as &mut dyn Read).collect();
            fqsort(&mut refs, 4, intcmp, &mut out, 256, "/tmp");
        }
        let mut f = rewind(out);

        let mut prev = i32::MIN;
        let mut nread = 0;
        let mut here = [0u8; 4];
        while fread_element(&mut f, &mut here) == 1 {
            let h = i32::from_ne_bytes(here);
            assert!(h >= prev);
            prev = h;
            nread += 1;
        }

        assert_eq!(nread, written);
    }
}
