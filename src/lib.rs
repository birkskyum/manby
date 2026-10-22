//! manby builds vector tilesets from geographic features, like tippecanoe, but
//! several times faster. See DESIGN.md.
//!
//! [`run`] takes [`Options`] and writes the tileset. Nothing in manby exits the
//! process or keeps global state; errors come back as [`Error`].

pub mod compress;
pub mod error;
pub mod ingest;
pub mod options;
pub mod order;
pub mod sink;
pub mod store;
pub mod tiler;

pub use error::{Error, Result};
pub use options::Options;

/// What a run produced.
#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub features: u64,
    pub tiles: u64,
    pub minzoom: i32,
    pub maxzoom: i32,
}

/// Reads the inputs, tiles them and writes the tileset that `options` describes.
pub fn run(options: &Options) -> Result<Summary> {
    todo!("manby::run: ingest, order, tile, write")
}
