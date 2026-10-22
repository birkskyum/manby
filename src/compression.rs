//! Port of compression.cpp: zlib streams inside the temporary geometry files.
//!
//! The struct fields are private to this module's port. tile.rs uses only the
//! methods below.

use std::fs::File;
use std::sync::atomic::AtomicI64;

pub struct Decompressor {}

impl Decompressor {
    pub fn new(f: File) -> Decompressor {
        todo!("Decompressor::new")
    }

    pub fn begin(&mut self) {
        todo!("Decompressor::begin")
    }

    /// Reads `size * nmemb` bytes into `p`. Returns the number of items read, like `fread`.
    pub fn fread(&mut self, p: &mut [u8], size: usize, nmemb: usize, geompos: &AtomicI64) -> i32 {
        todo!("Decompressor::fread")
    }

    pub fn end(&mut self, geompos: &AtomicI64) {
        todo!("Decompressor::end")
    }

    pub fn deserialize_ulong_long(&mut self, zigzag: &mut u64, geompos: &AtomicI64) -> i32 {
        todo!("Decompressor::deserialize_ulong_long")
    }

    pub fn deserialize_long_long(&mut self, n: &mut i64, geompos: &AtomicI64) -> i32 {
        todo!("Decompressor::deserialize_long_long")
    }

    pub fn deserialize_int(&mut self, n: &mut i32, geompos: &AtomicI64) -> i32 {
        todo!("Decompressor::deserialize_int")
    }

    pub fn deserialize_uint(&mut self, n: &mut u32, geompos: &AtomicI64) -> i32 {
        todo!("Decompressor::deserialize_uint")
    }
}

pub struct Compressor {}

impl Compressor {
    pub fn new(f: File) -> Compressor {
        todo!("Compressor::new")
    }

    pub fn begin(&mut self) {
        todo!("Compressor::begin")
    }

    pub fn end(&mut self, fpos: &AtomicI64, fname: &str) {
        todo!("Compressor::end")
    }

    pub fn fclose(&mut self) -> i32 {
        todo!("Compressor::fclose")
    }

    pub fn fwrite_check(
        &mut self,
        p: &[u8],
        size: usize,
        nmemb: usize,
        fpos: &AtomicI64,
        fname: &str,
    ) {
        todo!("Compressor::fwrite_check")
    }

    pub fn serialize_ulong_long(&mut self, val: u64, fpos: &AtomicI64, fname: &str) {
        todo!("Compressor::serialize_ulong_long")
    }

    pub fn serialize_long_long(&mut self, val: i64, fpos: &AtomicI64, fname: &str) {
        todo!("Compressor::serialize_long_long")
    }

    pub fn serialize_int(&mut self, val: i32, fpos: &AtomicI64, fname: &str) {
        todo!("Compressor::serialize_int")
    }

    pub fn serialize_uint(&mut self, val: u32, fpos: &AtomicI64, fname: &str) {
        todo!("Compressor::serialize_uint")
    }
}
