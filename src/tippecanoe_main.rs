//! Port of main.cpp, the `tippecanoe` program: reading the input, sorting the
//! features, and handing them to tile.rs.

use crate::serial::Reader;

/// `struct index`. The C++ packs `segment`, `t` and `seq` into the last 8 bytes
/// to keep the struct at 32 bytes, and so does this.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Index {
    pub start: i64,
    pub end: i64,
    pub ix: u64,
    /// segment in bits 0-15, t in bits 16-17, seq in bits 18-63
    bits: u64,
}

pub const INDEX_SIZE: usize = 32;

impl Index {
    pub fn segment(&self) -> i16 {
        self.bits as u16 as i16
    }

    pub fn set_segment(&mut self, segment: i16) {
        self.bits = (self.bits & !0xFFFF) | (segment as u16 as u64);
    }

    pub fn t(&self) -> u16 {
        ((self.bits >> 16) & 3) as u16
    }

    pub fn set_t(&mut self, t: u16) {
        self.bits = (self.bits & !(3 << 16)) | (((t as u64) & 3) << 16);
    }

    /// pack with segment and t to stay in 32 bytes
    pub fn seq(&self) -> u64 {
        self.bits >> 18
    }

    pub fn set_seq(&mut self, seq: u64) {
        self.bits = (self.bits & 0x3FFFF) | (seq << 18);
    }

    pub fn to_bytes(&self) -> [u8; INDEX_SIZE] {
        let mut out = [0u8; INDEX_SIZE];
        out[0..8].copy_from_slice(&self.start.to_ne_bytes());
        out[8..16].copy_from_slice(&self.end.to_ne_bytes());
        out[16..24].copy_from_slice(&self.ix.to_ne_bytes());
        out[24..32].copy_from_slice(&self.bits.to_ne_bytes());
        out
    }

    pub fn from_bytes(b: &[u8]) -> Index {
        Index {
            start: i64::from_ne_bytes(b[0..8].try_into().unwrap()),
            end: i64::from_ne_bytes(b[8..16].try_into().unwrap()),
            ix: u64::from_ne_bytes(b[16..24].try_into().unwrap()),
            bits: u64::from_ne_bytes(b[24..32].try_into().unwrap()),
        }
    }
}

pub fn checkdisk(r: &[Reader]) {
    todo!()
}

/// `progress_time()`: whether enough time has passed to print progress again.
pub fn progress_time() -> bool {
    todo!("tippecanoe_main::progress_time")
}

/// libc `mkstemp`. `name` is a template ending in XXXXXX, which is replaced
/// with the name of the file created. Returns the file descriptor, or -1.
pub fn mkstemp(name: &mut String) -> i32 {
    let mut buf = name.clone().into_bytes();
    buf.push(0);
    // SAFETY: buf is a writable NUL-terminated template.
    let fd = unsafe { libc::mkstemp(buf.as_mut_ptr() as *mut libc::c_char) };
    buf.pop();
    *name = String::from_utf8_lossy(&buf).into_owned();
    fd
}

/// `mkstemp_cloexec`. `name` is a template ending in XXXXXX, which is replaced
/// with the name of the file created. Returns the file descriptor, or -1.
pub fn mkstemp_cloexec(name: &mut String) -> i32 {
    todo!("tippecanoe_main::mkstemp_cloexec")
}

/// The `tippecanoe` program.
pub fn main() {
    todo!("tippecanoe_main::main")
}
