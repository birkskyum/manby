//! Tile compression. The default mode uses the system zlib with tippecanoe's
//! settings, because the tile size limit is checked on the compressed size.
//! Fast mode uses libdeflate.

/// Gzip-compresses a tile.
pub fn gzip(data: &[u8], fast: bool) -> Vec<u8> {
    todo!("compress::gzip")
}
