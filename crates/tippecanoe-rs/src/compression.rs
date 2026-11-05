//! Port of compression.hpp and compression.cpp: zlib streams inside the
//! temporary geometry files.
//!
//! The C++ structs expose their `FILE *` and `z_stream`, and tile.cpp reaches
//! into them. Here that access goes through `fp()`, `within`, `fseek()` and
//! `clear_avail()`.

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicI64, Ordering};

use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};

use crate::errors::EXIT_IMPOSSIBLE;

const Z_OK: i32 = 0;
const Z_STREAM_END: i32 = 1;
const Z_STREAM_ERROR: i32 = -2;
const Z_DATA_ERROR: i32 = -3;
const Z_BUF_ERROR: i32 = -5;

/// `::fread(buf, 1, buf.size(), fp)`: fills `buf` unless the file ends first.
fn fread_full(fp: &mut File, buf: &mut [u8]) -> usize {
    let mut got = 0;
    while got < buf.len() {
        match fp.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    got
}

pub struct Decompressor {
    fp: File,
    zs: Option<Decompress>,
    msg: String,
    buf: Vec<u8>,
    next_in: usize,
    avail_in: usize,

    /// from begin() to receiving end-of-stream
    pub within: bool,
}

impl Decompressor {
    pub fn new(f: File) -> Decompressor {
        Decompressor {
            fp: f,
            zs: None,
            msg: String::new(),
            buf: vec![0u8; 5000],
            next_in: 0,
            avail_in: 0,
            within: false,
        }
    }

    /// `inflate(&zs, Z_NO_FLUSH)` from the buffered input into `out`. Returns the
    /// zlib status and the number of bytes consumed and produced.
    fn inflate(&mut self, out: &mut [u8]) -> (i32, usize, usize) {
        let zs = match self.zs.as_mut() {
            Some(zs) => zs,
            None => return (Z_STREAM_ERROR, 0, 0),
        };

        let before_in = zs.total_in();
        let before_out = zs.total_out();
        let input = &self.buf[self.next_in..self.next_in + self.avail_in];
        let d = match zs.decompress(input, out, FlushDecompress::None) {
            Ok(Status::Ok) => Z_OK,
            Ok(Status::StreamEnd) => Z_STREAM_END,
            Ok(Status::BufError) => Z_BUF_ERROR,
            Err(e) => {
                self.msg = e.message().unwrap_or("").to_string();
                Z_DATA_ERROR
            }
        };
        let used = (zs.total_in() - before_in) as usize;
        let made = (zs.total_out() - before_out) as usize;

        self.next_in += used;
        self.avail_in -= used;
        (d, used, made)
    }

    pub fn begin(&mut self) {
        self.within = true;
        self.msg = String::new();

        // inflateInit
        self.zs = Some(Decompress::new(true));
    }

    /// Reads `size * nmemb` bytes into `p`. Returns the number of items read, like `fread`.
    pub fn fread(&mut self, p: &mut [u8], size: usize, nmemb: usize, geompos: &AtomicI64) -> i32 {
        let total = size * nmemb;
        let mut out = 0;

        while out < total {
            if self.avail_in == 0 {
                let n = fread_full(&mut self.fp, &mut self.buf);
                if n == 0 {
                    if self.within {
                        eprintln!("Reached EOF while decompressing");
                        std::process::exit(EXIT_IMPOSSIBLE);
                    } else {
                        break;
                    }
                }
                self.next_in = 0;
                self.avail_in = n;
            }

            if self.within {
                let (d, used, made) = self.inflate(&mut p[out..total]);
                geompos.fetch_add(used as i64, Ordering::Relaxed);
                out += made;

                if d == Z_OK {
                    // it made some progress
                } else if d == Z_STREAM_END {
                    // it may have made some progress and now we are done
                    self.within = false;
                    break;
                } else {
                    eprintln!("decompression error {} {}", d, self.msg);
                    std::process::exit(EXIT_IMPOSSIBLE);
                }
            } else {
                let n = std::cmp::min(self.avail_in, total - out);
                p[out..out + n].copy_from_slice(&self.buf[self.next_in..self.next_in + n]);
                geompos.fetch_add(n as i64, Ordering::Relaxed);

                out += n;
                self.avail_in -= n;
                self.next_in += n;
            }
        }

        (out / size) as i32
    }

    pub fn end(&mut self, geompos: &AtomicI64) {
        // "within" means that we haven't received end-of-stream yet,
        // so consume more compressed data until we get there.
        // This can be necessary if the caller knows that it is at
        // the end of the feature stream (because it got a 0-length
        // feature) but the decompressor doesn't know yet.

        if self.within {
            loop {
                if self.avail_in == 0 {
                    let n = fread_full(&mut self.fp, &mut self.buf);
                    self.next_in = 0;
                    self.avail_in = n;
                }

                let (d, used, _) = self.inflate(&mut []);
                geompos.fetch_add(used as i64, Ordering::Relaxed);

                if d == Z_STREAM_END {
                    break;
                }
                if d == Z_OK {
                    continue;
                }

                eprintln!("decompression: got {}, not Z_STREAM_END", d);
                std::process::exit(EXIT_IMPOSSIBLE);
            }

            self.within = false;
        }

        // inflateEnd
        if self.zs.take().is_none() {
            eprintln!("end decompression: {} {}", Z_STREAM_ERROR, self.msg);
            std::process::exit(EXIT_IMPOSSIBLE);
        }
    }

    pub fn deserialize_ulong_long(&mut self, zigzag: &mut u64, geompos: &AtomicI64) -> i32 {
        *zigzag = 0;
        let mut shift: u32 = 0;

        loop {
            let mut c = [0u8; 1];
            if self.fread(&mut c, 1, 1, geompos) != 1 {
                return 0;
            }
            let c = c[0];

            if (c & 0x80) == 0 {
                *zigzag |= (c as u64).wrapping_shl(shift);
                break;
            } else {
                *zigzag |= ((c & 0x7F) as u64).wrapping_shl(shift);
                shift += 7;
            }
        }

        1
    }

    pub fn deserialize_long_long(&mut self, n: &mut i64, geompos: &AtomicI64) -> i32 {
        let mut zigzag: u64 = 0;
        let ret = self.deserialize_ulong_long(&mut zigzag, geompos);
        *n = crate::serial::decode_zigzag64(zigzag);
        ret
    }

    pub fn deserialize_int(&mut self, n: &mut i32, geompos: &AtomicI64) -> i32 {
        let mut ll: i64 = 0;
        let ret = self.deserialize_long_long(&mut ll, geompos);
        *n = ll as i32;
        ret
    }

    pub fn deserialize_uint(&mut self, n: &mut u32, geompos: &AtomicI64) -> i32 {
        let mut v: u64 = 0;
        self.deserialize_ulong_long(&mut v, geompos);
        *n = v as u32;
        1
    }

    /// `fseek(geoms->fp, off, SEEK_SET)`: 0 on success, -1 on failure.
    pub fn fseek(&mut self, off: i64) -> i32 {
        if off < 0 {
            crate::cstd::set_errno(libc::EINVAL);
            return -1;
        }
        match self.fp.seek(SeekFrom::Start(off as u64)) {
            Ok(_) => 0,
            Err(e) => {
                crate::cstd::set_errno(e.raw_os_error().unwrap_or(libc::EINVAL));
                -1
            }
        }
    }

    /// `zs.avail_in = 0; zs.avail_out = 0;`, after seeking elsewhere.
    pub fn clear_avail(&mut self) {
        self.avail_in = 0;
    }

    /// The underlying file, the C++ `fp`.
    pub fn fp(&mut self) -> &mut File {
        &mut self.fp
    }
}

pub struct Compressor {
    fp: Option<BufWriter<File>>,
    zs: Option<Compress>,
    msg: String,
}

impl Compressor {
    pub fn new(f: File) -> Compressor {
        Compressor {
            fp: Some(BufWriter::new(f)),
            zs: None,
            msg: String::new(),
        }
    }

    /// The underlying buffered file, the C++ `fp`, for writing uncompressed
    /// data between the compressed streams.
    pub fn fp(&mut self) -> &mut BufWriter<File> {
        self.fp.as_mut().expect("compressor already closed")
    }

    /// `deflate(&zs, flush)` from `input` into `out`. Returns the zlib status and
    /// the number of bytes consumed and produced.
    fn deflate(
        &mut self,
        input: &[u8],
        out: &mut [u8],
        flush: FlushCompress,
    ) -> (i32, usize, usize) {
        let zs = match self.zs.as_mut() {
            Some(zs) => zs,
            None => return (Z_STREAM_ERROR, 0, 0),
        };

        let before_in = zs.total_in();
        let before_out = zs.total_out();
        let d = match zs.compress(input, out, flush) {
            Ok(Status::Ok) => Z_OK,
            Ok(Status::StreamEnd) => Z_STREAM_END,
            Ok(Status::BufError) => Z_BUF_ERROR,
            Err(e) => {
                self.msg = e.message().unwrap_or("").to_string();
                Z_STREAM_ERROR
            }
        };
        let used = (zs.total_in() - before_in) as usize;
        let made = (zs.total_out() - before_out) as usize;
        (d, used, made)
    }

    pub fn begin(&mut self) {
        self.msg = String::new();

        // deflateInit(&zs, Z_DEFAULT_COMPRESSION)
        self.zs = Some(Compress::new(Compression::default(), true));
    }

    pub fn end(&mut self, fpos: &AtomicI64, fname: &str) {
        let mut buf = vec![0u8; 5000];

        loop {
            let (d, _, made) = self.deflate(&[], &mut buf, FlushCompress::Finish);
            crate::serial::fwrite_check(&buf[..made], 1, made, self.fp(), fpos, fname);

            if d == Z_OK || d == Z_BUF_ERROR {
                // it can take several calls to flush out all the buffered data
                continue;
            }

            if d != Z_STREAM_END {
                eprintln!("{}: finish compression: {} {}", fname, d, self.msg);
                std::process::exit(EXIT_IMPOSSIBLE);
            }

            break;
        }

        // deflateEnd
        if self.zs.take().is_none() {
            eprintln!(
                "{}: end compression: {} {}",
                fname, Z_STREAM_ERROR, self.msg
            );
            std::process::exit(EXIT_IMPOSSIBLE);
        }

        crate::serial::fwrite_check(&buf[..0], 1, 0, self.fp(), fpos, fname);
    }

    pub fn fclose(&mut self) -> i32 {
        match self.fp.take() {
            Some(fp) => match fp.into_inner() {
                Ok(_) => 0,
                Err(e) => {
                    crate::cstd::set_errno(e.error().raw_os_error().unwrap_or(libc::EIO));
                    -1
                }
            },
            None => {
                crate::cstd::set_errno(libc::EBADF);
                -1
            }
        }
    }

    pub fn fwrite_check(
        &mut self,
        p: &[u8],
        size: usize,
        nmemb: usize,
        fpos: &AtomicI64,
        fname: &str,
    ) {
        let mut buf = vec![0u8; size * nmemb * 2 + 200];

        let mut next_in = 0;
        let mut avail_in = size * nmemb;

        while avail_in > 0 {
            let (d, used, made) = self.deflate(
                &p[next_in..next_in + avail_in],
                &mut buf,
                FlushCompress::None,
            );
            next_in += used;
            avail_in -= used;

            if d != Z_OK {
                eprintln!("{}: deflate: {} {}", fname, d, self.msg);
                std::process::exit(EXIT_IMPOSSIBLE);
            }

            crate::serial::fwrite_check(&buf[..made], 1, made, self.fp(), fpos, fname);
        }
    }

    pub fn serialize_ulong_long(&mut self, val: u64, fpos: &AtomicI64, fname: &str) {
        let mut val = val;
        loop {
            let mut b = (val & 0x7F) as u8;
            if (val >> 7) != 0 {
                b |= 0x80;
                self.fwrite_check(&[b], 1, 1, fpos, fname);
                val >>= 7;
            } else {
                self.fwrite_check(&[b], 1, 1, fpos, fname);
                break;
            }
        }
    }

    pub fn serialize_long_long(&mut self, val: i64, fpos: &AtomicI64, fname: &str) {
        let zigzag = crate::serial::encode_zigzag64(val);

        self.serialize_ulong_long(zigzag, fpos, fname);
    }

    pub fn serialize_int(&mut self, val: i32, fpos: &AtomicI64, fname: &str) {
        self.serialize_long_long(val as i64, fpos, fname);
    }

    pub fn serialize_uint(&mut self, val: u32, fpos: &AtomicI64, fname: &str) {
        self.serialize_ulong_long(val as u64, fpos, fname);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::FromRawFd;

    fn tmpfile() -> File {
        let mut name = String::from("/tmp/compression-test.XXXXXX");
        let fd = crate::tippecanoe_main::mkstemp_cloexec(&mut name);
        assert!(fd >= 0);
        std::fs::remove_file(&name).unwrap();
        // SAFETY: mkstemp returned a new descriptor, which the File now owns.
        unsafe { File::from_raw_fd(fd) }
    }

    #[test]
    fn round_trip() {
        let f = tmpfile();
        let reader = f.try_clone().unwrap();
        let pos = AtomicI64::new(0);

        let mut c = Compressor::new(f);
        crate::serial::fserialize_int(c.fp(), 7, &pos, "test");
        c.begin();
        for i in 0..1000i64 {
            c.serialize_long_long(i * 1000 - 50000, &pos, "test");
        }
        c.fwrite_check(b"some feature bytes", 1, 18, &pos, "test");
        c.serialize_uint(0, &pos, "test");
        c.end(&pos, "test");
        crate::serial::fserialize_int(c.fp(), 8, &pos, "test");
        assert_eq!(c.fclose(), 0);

        let mut reader = reader;
        reader.seek(SeekFrom::Start(0)).unwrap();
        let inpos = AtomicI64::new(0);
        let mut d = Decompressor::new(reader);

        let mut n = 0i32;
        assert_eq!(d.deserialize_int(&mut n, &inpos), 1);
        assert_eq!(n, 7);
        d.begin();
        for i in 0..1000i64 {
            let mut v = 0i64;
            assert_eq!(d.deserialize_long_long(&mut v, &inpos), 1);
            assert_eq!(v, i * 1000 - 50000);
        }
        let mut bytes = [0u8; 18];
        assert_eq!(d.fread(&mut bytes, 1, 18, &inpos), 18);
        assert_eq!(&bytes, b"some feature bytes");
        let mut z = 1u32;
        d.deserialize_uint(&mut z, &inpos);
        assert_eq!(z, 0);
        d.end(&inpos);
        assert!(!d.within);
        assert_eq!(d.deserialize_int(&mut n, &inpos), 1);
        assert_eq!(n, 8);
        assert_eq!(inpos.load(Ordering::Relaxed), pos.load(Ordering::Relaxed));
        assert_eq!(d.deserialize_int(&mut n, &inpos), 0);
    }
}
