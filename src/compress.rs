//! Tile compression: the system zlib with tippecanoe's settings, where the
//! tile size limit needs the size zlib makes, and libdeflate at its default
//! level 6 otherwise, which is smaller than zlib -6 on tiles and 2 to 4 times
//! faster.
//!
//! The default output is byte for byte what tippecanoe-rs's `mvt::compress`
//! makes: gzip framing, level 6, a 15-bit window, memLevel 8 and the default
//! strategy, with the same zlib. zlib's output does not depend on how the
//! output buffer is split up, so one call with a large enough buffer gives the
//! same bytes as tippecanoe's loop that grows the buffer in steps.

use std::cell::RefCell;

use flate2::{Compress, Compression, FlushCompress, Status};
use libdeflater::CompressionLvl;

/// A gzip compressor that keeps its zlib and libdeflate state between calls,
/// so compressing a tile neither allocates nor clears that state again.
/// Use one per thread.
#[derive(Default)]
pub struct Gzip {
    zlib: Option<Compress>,
    libdeflate: Option<libdeflater::Compressor>,
}

impl Gzip {
    pub fn new() -> Gzip {
        Gzip::default()
    }

    /// Compresses `data` into `out` with libdeflate or zlib, replacing what
    /// `out` held.
    pub fn compress_into(&mut self, data: &[u8], libdeflate: bool, out: &mut Vec<u8>) {
        out.clear();
        if libdeflate {
            let c = self
                .libdeflate
                .get_or_insert_with(|| libdeflater::Compressor::new(CompressionLvl::default()));
            out.resize(c.gzip_compress_bound(data.len()), 0);
            let n = c
                .gzip_compress(data, out)
                .expect("the buffer has room for the compression bound");
            out.truncate(n);
            return;
        }

        let c = self
            .zlib
            .get_or_insert_with(|| Compress::new_gzip(Compression::new(6), 15));
        c.reset();
        // deflateBound() for these settings is a little over this.
        out.reserve(data.len() + (data.len() >> 10) + 64);
        loop {
            let consumed = c.total_in() as usize;
            let status = c
                .compress_vec(&data[consumed..], out, FlushCompress::Finish)
                .expect("deflate cannot fail with these settings");
            if status == Status::StreamEnd {
                break;
            }
            out.reserve(out.capacity().max(1024));
        }
    }

    /// Returns `data` compressed.
    pub fn compress(&mut self, data: &[u8], libdeflate: bool) -> Vec<u8> {
        let mut out = Vec::new();
        self.compress_into(data, libdeflate, &mut out);
        out
    }
}

thread_local! {
    // Scratch state only: what it holds never changes a result.
    static GZIP: RefCell<Gzip> = RefCell::new(Gzip::new());
}

/// Gzip-compresses a tile, reusing a compressor kept per thread.
pub fn gzip(data: &[u8], libdeflate: bool) -> Vec<u8> {
    GZIP.with(|g| g.borrow_mut().compress(data, libdeflate))
}

/// The most bytes zlib makes from `n` bytes at tippecanoe's settings:
/// zlib's `deflateBound()` for a gzip stream with the default window and
/// memLevel, which is the same in zlib 1.2.12 and 1.3. Whatever the content,
/// a tile of `n` bytes compresses to at most this many.
pub fn zlib_bound(n: usize) -> usize {
    n + (n >> 12) + (n >> 14) + (n >> 25) + 13 - 6 + 18
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn tippecanoe_gzip(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        tippecanoe::mvt::compress(data, &mut out, true);
        out
    }

    fn samples() -> Vec<Vec<u8>> {
        let mut seed: u64 = 7;
        let mut next = move || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as u8
        };
        let mut v = vec![
            Vec::new(),
            b"a".to_vec(),
            b"hello hello hello hello".to_vec(),
        ];
        for &n in &[100usize, 1000, 4096, 65536, 300_000, 3_000_000] {
            v.push((0..n).map(|_| next()).collect());
            v.push((0..n).map(|i| (i % 251) as u8 ^ (next() & 3)).collect());
            v.push(vec![0u8; n]);
        }
        v
    }

    #[test]
    fn same_bytes_as_tippecanoe() {
        let mut g = Gzip::new();
        for data in samples() {
            let expected = tippecanoe_gzip(&data);
            assert_eq!(g.compress(&data, false), expected, "{} bytes", data.len());
            assert_eq!(gzip(&data, false), expected, "{} bytes", data.len());
        }
    }

    #[test]
    fn libdeflate_round_trips() {
        let mut g = Gzip::new();
        for data in samples() {
            let z = g.compress(&data, true);
            let mut back = Vec::new();
            flate2::read::GzDecoder::new(&z[..])
                .read_to_end(&mut back)
                .unwrap();
            assert_eq!(back, data);
        }
    }

    unsafe extern "C" fn zalloc(
        _: *mut std::ffi::c_void,
        items: u32,
        size: u32,
    ) -> *mut std::ffi::c_void {
        // SAFETY: calloc has no preconditions.
        unsafe { libc::calloc(items as usize, size as usize) }
    }

    unsafe extern "C" fn zfree(_: *mut std::ffi::c_void, p: *mut std::ffi::c_void) {
        // SAFETY: zlib frees only what zalloc returned.
        unsafe { libc::free(p) }
    }

    #[test]
    fn bound_is_zlibs_own() {
        // SAFETY: the stream is set up by deflateInit2_ before deflateBound
        // reads it, and ended once.
        unsafe {
            let mut strm = libz_sys::z_stream {
                next_in: std::ptr::null_mut(),
                avail_in: 0,
                total_in: 0,
                next_out: std::ptr::null_mut(),
                avail_out: 0,
                total_out: 0,
                msg: std::ptr::null_mut(),
                state: std::ptr::null_mut(),
                zalloc,
                zfree,
                opaque: std::ptr::null_mut(),
                data_type: 0,
                adler: 0,
                reserved: 0,
            };
            let version = libz_sys::zlibVersion();
            assert_eq!(
                libz_sys::deflateInit2_(
                    &mut strm,
                    6,
                    libz_sys::Z_DEFLATED,
                    31,
                    8,
                    libz_sys::Z_DEFAULT_STRATEGY,
                    version,
                    std::mem::size_of::<libz_sys::z_stream>() as i32,
                ),
                libz_sys::Z_OK
            );
            let mut n = 0usize;
            while n < 100_000_000 {
                assert_eq!(
                    libz_sys::deflateBound(&mut strm, n as libz_sys::uLong) as usize,
                    zlib_bound(n),
                    "{} bytes",
                    n
                );
                n = n * 3 / 2 + 1;
            }
            libz_sys::deflateEnd(&mut strm);
        }
    }

    #[test]
    fn compresses_within_the_bound() {
        let mut g = Gzip::new();
        for data in samples() {
            assert!(g.compress(&data, false).len() <= zlib_bound(data.len()));
        }
    }
}
