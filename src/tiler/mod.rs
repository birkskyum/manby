//! Making the tiles: a parallel walk of the tile quadtree. See DESIGN.md.

use crate::options::Options;
use crate::order::Plan;
use crate::sink::TileSink;
use crate::store::FeatureStore;

/// What the tiler reports back, for the metadata and the summary.
#[derive(Clone, Debug, Default)]
pub struct TileStats {
    pub tiles: u64,
    /// tippecanoe's per-zoom strategies, for the metadata's `strategies` entry.
    pub strategies: Vec<tippecanoe::tile::Strategy>,
    /// The zoom level actually reached, which can be lower than planned.
    pub maxzoom: i32,
}

/// Makes every tile of `plan` from `store` and hands them to `sink`.
pub fn tile(
    store: &FeatureStore,
    plan: &Plan,
    options: &Options,
    sink: &dyn TileSink,
) -> crate::Result<TileStats> {
    todo!("tiler::tile")
}
