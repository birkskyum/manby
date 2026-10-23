//! What tile-join compares when two inputs have a tile at the same z/x/y:
//! the tiles' bytes, so that the one whose stored bytes sort first is
//! appended first (`tileset_reader::operator<`).
//!
//! manby stores most tiles compressed with libdeflate rather than zlib, so its
//! tiles' bytes are not the bytes tippecanoe would have stored, and they sort
//! differently. A tile whose gzip header says libdeflate made it (OS byte
//! 255, where zlib writes its own OS code) in a tileset whose metadata says
//! tippecanoe or tile-join made it can only have come from manby, which makes
//! the same uncompressed tile as tippecanoe and only compresses it otherwise.
//! With [`TieBreak::Zlib`], such a tile is compared by the bytes zlib makes of
//! it with tippecanoe's settings, which are the bytes tippecanoe would have
//! stored. Tiles of other tilesets, and zlib's own tiles, are compared by
//! their bytes as stored, as tile-join compares them.

use std::cmp::Ordering;
use std::sync::{Arc, OnceLock};

use super::tile::decompress;

/// How tiles at the same z/x/y are ordered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TieBreak {
    /// By the bytes zlib would have stored for tiles manby compressed with
    /// libdeflate, and by the stored bytes otherwise, so that manby and then
    /// `manby join` make the tiles tippecanoe and then tile-join make.
    #[default]
    Zlib,
    /// By the stored bytes, as tile-join itself orders them.
    Bytes,
}

/// The bytes a tile is compared by.
pub enum Key {
    Bytes(Arc<[u8]>),
    /// The bytes zlib makes of `content`, worked out when the gzip header
    /// does not decide the comparison.
    Zlib {
        content: Vec<u8>,
        bytes: OnceLock<Vec<u8>>,
    },
}

/// The gzip header zlib writes with tippecanoe's settings, which starts
/// every tile tippecanoe compresses.
pub fn zlib_header() -> &'static [u8] {
    static HEADER: OnceLock<Vec<u8>> = OnceLock::new();
    HEADER.get_or_init(|| {
        let mut z = crate::compress::gzip(b"", false);
        z.truncate(10);
        z
    })
}

/// The gzip header libdeflate writes at level 6.
pub const LIBDEFLATE_HEADER: [u8; 10] = [0x1F, 0x8B, 8, 0, 0, 0, 0, 0, 0, 0xFF];

/// Whether `data` starts with the gzip header libdeflate writes at level 6.
pub fn is_libdeflate_gzip(data: &[u8]) -> bool {
    data.len() >= 18
        && data[0] == 0x1F
        && data[1] == 0x8B
        && data[2] == 8
        && data[3] == 0
        && data[8] == 0
        && data[9] == 0xFF
}

impl Key {
    /// The key of a tile with `data`, for a tileset that tippecanoe or
    /// tile-join made if `tippecanoe_made`.
    pub fn new(data: Arc<[u8]>, tippecanoe_made: bool, tie_break: TieBreak) -> Key {
        if tie_break == TieBreak::Zlib && tippecanoe_made && is_libdeflate_gzip(&data) {
            let mut d = libdeflater::Decompressor::new();
            let mut content = Vec::new();
            if decompress(&mut d, &data, &mut content) {
                return Key::Zlib {
                    content,
                    bytes: OnceLock::new(),
                };
            }
        }
        Key::Bytes(data)
    }

    /// The bytes known without compressing anything.
    fn known(&self) -> &[u8] {
        match self {
            Key::Bytes(b) => b,
            Key::Zlib { bytes, .. } => match bytes.get() {
                Some(b) => b,
                None => zlib_header(),
            },
        }
    }

    fn all(&self) -> &[u8] {
        match self {
            Key::Bytes(b) => b,
            Key::Zlib { content, bytes } => {
                bytes.get_or_init(|| crate::compress::gzip(content, false))
            }
        }
    }

    fn is_complete(&self) -> bool {
        match self {
            Key::Bytes(_) => true,
            Key::Zlib { bytes, .. } => bytes.get().is_some(),
        }
    }
}

/// `std::string::operator<` on the keys' bytes, compressing only when the
/// bytes known so far do not decide.
pub fn cmp(a: &Key, b: &Key) -> Ordering {
    let (ka, kb) = (a.known(), b.known());
    let n = ka.len().min(kb.len());
    match ka[..n].cmp(&kb[..n]) {
        Ordering::Equal => {}
        o => return o,
    }
    if a.is_complete() && b.is_complete() {
        return ka.len().cmp(&kb.len());
    }
    a.all().cmp(b.all())
}
