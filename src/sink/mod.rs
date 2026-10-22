//! Where finished tiles go. See DESIGN.md.

pub mod dir;
pub mod mbtiles;
pub mod pmtiles;

use tippecanoe::mbtiles::Metadata;

/// Receives finished tiles from any number of threads, in any order.
pub trait TileSink: Send + Sync {
    /// `data` is the tile as it should be stored (compressed unless tile
    /// compression is off).
    fn write_tile(&self, z: u8, x: u32, y: u32, data: &[u8]) -> crate::Result<()>;

    /// Removes the tiles of zoom `z` already written, for tippecanoe's retries
    /// that redo a whole zoom level with a lower maximum zoom.
    fn erase_zoom(&self, z: u8) -> crate::Result<()>;

    /// Writes the metadata and closes the output.
    fn finish(self: Box<Self>, metadata: &Metadata) -> crate::Result<()>;
}
