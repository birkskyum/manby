//! manby builds vector tilesets from geographic features, like tippecanoe, but
//! several times faster. See DESIGN.md.
//!
//! [`run`] takes [`Options`] and writes the tileset, and [`join()`] takes
//! [`JoinOptions`] and joins tilesets as tippecanoe's tile-join does. Nothing
//! in manby exits the process; errors come back as [`Error`], with the exit
//! status tippecanoe would use.
//!
//! The options are tippecanoe's, so its command line is the shortest way to
//! them:
//!
//! ```no_run
//! let args = ["manby", "-zg", "--drop-densest-as-needed", "-o", "places.pmtiles", "places.geojson"];
//! let options = manby::options::parse_args(&args.map(String::from))?;
//! let summary = manby::run(&options)?;
//! println!("{} tiles, to zoom {}", summary.tiles, summary.maxzoom);
//! # Ok::<(), manby::Error>(())
//! ```
//!
//! They are also fields, set on the defaults:
//!
//! ```no_run
//! use manby::options::Output;
//!
//! let mut options = manby::Options::default();
//! options.inputs = vec!["places.geojson".to_string()];
//! options.output = Output::Pmtiles("places.pmtiles".to_string());
//! options.maxzoom = 12;
//! manby::run(&options)?;
//! # Ok::<(), manby::Error>(())
//! ```
//!
//! # What is public
//!
//! The API is what these pages show: [`run`] and [`join()`] with their options
//! and summaries, and the commands in [`stats`], [`serve`] and [`overzoom`].
//! The options and summaries are `#[non_exhaustive]`, since each new option
//! is a new field. Some fields have types of tippecanoe-rs, which is
//! [`tippecanoe`] here.
//!
//! manby's other modules are public for its own tests, examples and
//! benchmarks. They are hidden from these pages and can change in any
//! release.
//!
//! # One run at a time
//!
//! The code that manby borrows from tippecanoe-rs reads its options from
//! globals, which each run sets, and the memory budget is the process's. Runs
//! in one process should take turns unless their options are the same.

#[doc(hidden)]
pub mod buf;
#[doc(hidden)]
pub mod clean;
#[doc(hidden)]
pub mod command;
#[doc(hidden)]
pub mod compress;
#[doc(hidden)]
pub mod crs;
pub mod error;
#[doc(hidden)]
pub mod filter;
#[doc(hidden)]
pub mod ingest;
pub mod join;
#[doc(hidden)]
pub mod mem;
#[doc(hidden)]
pub mod mlt;
pub mod options;
#[doc(hidden)]
pub mod order;
pub mod overzoom;
mod pool;
pub mod serve;
pub mod shard;
#[doc(hidden)]
pub mod sink;
pub mod stats;
#[doc(hidden)]
pub mod store;
#[doc(hidden)]
pub mod tiler;

/// tippecanoe-rs, for the types of its that the options have.
pub use tippecanoe;

use std::collections::BTreeMap;

use tippecanoe::errors::EXIT_NODATA;
use tippecanoe::mbtiles::{LayermapEntry, Metadata, make_metadata, merge_layermaps};
use tippecanoe::options::{P_TILE_COMPRESSION, P_TILE_STATS};
use tippecanoe::projection::tile2lonlat;

pub use error::{Error, Result};
pub use filter::Filter;
pub use join::{JoinOptions, JoinSummary, join};
pub use mlt::{MltProfile, TileFormat};
pub use options::Options;
pub use shard::{MergeOptions, MergeSummary, merge};

/// What a run produced.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Summary {
    /// Features read.
    pub features: u64,
    /// Tiles written.
    pub tiles: u64,
    /// The lowest zoom written.
    pub minzoom: i32,
    /// The highest zoom written.
    pub maxzoom: i32,
    /// A tile could not be made small enough, so the tileset stops at
    /// `maxzoom`, below the zoom that was planned. tippecanoe exits with
    /// `EXIT_INCOMPLETE` then.
    pub incomplete: bool,
}

/// The tiler's options for a run.
#[doc(hidden)]
pub fn tile_options(
    o: &Options,
    plan: &order::Plan,
    store: &store::FeatureStore,
) -> tiler::TileOptions {
    tiler::TileOptions::new(o, plan, store)
}

/// Reads the inputs, tiles them and writes the tileset that `options` describes.
pub fn run(options: &Options) -> Result<Summary> {
    let mut o = options.clone();
    for w in o.resolve()? {
        eprintln!("{}", w);
    }
    // TRANSITIONAL: see set_tippecanoe_globals.
    options::set_tippecanoe_globals(&o, o.cluster_maxzoom);
    let budget = o.memory_budget();
    buf::set_budget(budget, std::path::Path::new(&o.tmpdir));

    let sink = sink::open(
        &o.output,
        &sink::SinkSettings {
            force: o.force,
            allow_existing: o.forcetable,
            tile_compression: !o.prevent.get(P_TILE_COMPRESSION),
            tmpdir: o.tmpdir.clone().into(),
            memory_budget: (budget / 64).clamp(1 << 20, 16 << 20) as usize,
            program: o.program.clone(),
            tile_format: o.tile_format,
            ..Default::default()
        },
    )?;

    let mut store = ingest::read_inputs(&o)?;
    let plan = order::order(&mut store, &o)?;
    options::set_tippecanoe_globals(&o, plan.cluster_maxzoom);

    let mut topts = tile_options(&o, &plan, &store);
    let split = o.shard.map(|s| {
        let zoom = shard::pivot(&o, &plan);
        let ranges = shard::split(&store, &o, &plan, zoom, s.count);
        let (lo, hi) = ranges[s.index as usize];
        topts.shard = Some(shard::Range {
            index: s.index,
            zoom,
            lo,
            hi,
            minzoom: plan.minzoom,
        });
        (s, zoom, ranges)
    });
    let stats = tiler::tile_with(&store, &topts, sink.as_ref())?;

    // The tiler can also extend the zoom levels beyond the plan.
    let minzoom = plan.minzoom;
    let maxzoom = stats.maxzoom;
    let incomplete = stats.incomplete;
    if incomplete {
        if maxzoom > minzoom {
            eprintln!(
                "\n\n\n*** NOTE TILES ONLY COMPLETE THROUGH ZOOM {} ***",
                maxzoom
            );
        } else {
            return Err(Error::new(
                EXIT_NODATA,
                format!("{}: No zoom levels were successfully written", o.program),
            ));
        }
    }

    let inputs = MetaInputs::new(&o, &plan, &store);
    let metadata = inputs.metadata(&o.tileset_name(), &stats);
    match split {
        Some((s, zoom, ranges)) => {
            let tiling = stats.shard.clone().unwrap_or_default();
            if !tiling.again.is_empty() {
                eprintln!("{}", shard::again_note(s, &tiling.again));
            }
            let record = shard::Record {
                index: s.index,
                count: s.count,
                zoom,
                fingerprint: shard::fingerprint(&inputs, &plan, &store, zoom, &ranges),
                ranges,
                name: o.name.clone(),
                meta: inputs,
                tiling,
            };
            let mut extra = record.meta.extra();
            extra.push((shard::KEY.to_string(), record.to_json()));
            sink.finish_with(&metadata, &extra)?;
        }
        None => sink.finish_with(&metadata, &inputs.extra())?,
    }

    Ok(Summary {
        features: store.features.len() as u64,
        tiles: stats.tiles,
        minzoom,
        maxzoom,
        incomplete,
    })
}

/// The metadata's key for `--h3`'s resolutions and zooms.
pub(crate) const H3_KEY: &str = "manby_h3";

/// What a run's metadata is made of besides its name and what the tiling
/// found, which `manby merge` gets from each shard.
#[doc(hidden)]
#[derive(Clone, Debug, Default)]
pub struct MetaInputs {
    pub minzoom: i32,
    /// The input's bounding boxes in world coordinates: the whole one, and
    /// the two that put the antimeridian in different places.
    pub bbox: [i64; 4],
    pub bbox1: [i64; 4],
    pub bbox2: [i64; 4],
    pub attribution: Option<String>,
    pub description: Option<String>,
    pub tilestats: bool,
    pub attribute_descriptions: BTreeMap<String, String>,
    pub commandline: String,
    pub basezoom: i32,
    pub droprate: f64,
    pub retain_points_multiplier: i32,
    /// The input's layers.
    pub layermap: BTreeMap<String, LayermapEntry>,
    /// How the output stores its tiles.
    pub tile_compression: bool,
    pub tile_format: TileFormat,
    /// `--h3`: the hexagons' resolutions and zooms, as JSON, for
    /// `manby serve`'s map.
    pub h3: Option<String>,
}

impl MetaInputs {
    pub fn new(o: &Options, plan: &order::Plan, store: &store::FeatureStore) -> MetaInputs {
        MetaInputs {
            minzoom: plan.minzoom,
            bbox: store.bbox,
            bbox1: store.bbox1,
            bbox2: store.bbox2,
            attribution: o.attribution.clone(),
            description: o.description.clone(),
            tilestats: !o.prevent.get(P_TILE_STATS),
            attribute_descriptions: o.attribute_descriptions.clone(),
            commandline: o.commandline.clone(),
            basezoom: plan.basezoom,
            droprate: plan.droprate,
            retain_points_multiplier: o.retain_points_multiplier,
            layermap: store.layermap(),
            tile_compression: !o.prevent.get(P_TILE_COMPRESSION),
            tile_format: o.tile_format,
            h3: o.h3.then(|| ingest::h3::Setup::new(o).json()),
        }
    }

    /// What the metadata has beyond tippecanoe's: `--h3`'s resolutions and
    /// zooms, as `manby_h3`.
    pub fn extra(&self) -> Vec<(String, String)> {
        self.h3
            .iter()
            .map(|h| (H3_KEY.to_string(), h.clone()))
            .collect()
    }

    /// The tileset's metadata, as tippecanoe makes it.
    pub fn metadata(&self, name: &str, stats: &tiler::TileStats) -> Metadata {
        let maxzoom = stats.maxzoom;

        // mbtiles-style bounding box and center
        let (midx, midy) = (stats.midx, stats.midy);
        let (mut minlon, mut maxlat) = tile2lonlat(midx as i64, midy as i64, maxzoom);
        let (mut maxlon, mut minlat) = tile2lonlat(
            midx.wrapping_add(1) as i64,
            midy.wrapping_add(1) as i64,
            maxzoom,
        );

        let mut midlat = (maxlat + minlat) / 2.0;
        let mut midlon = (maxlon + minlon) / 2.0;

        let b = self.bbox;
        (minlon, maxlat) = tile2lonlat(b[0], b[1], 32);
        (maxlon, minlat) = tile2lonlat(b[2], b[3], 32);

        midlat = midlat.clamp(minlat, maxlat);
        midlon = midlon.clamp(minlon, maxlon);

        // antimeridian-aware bounding box: whichever of the two is narrower
        let (b1, b2) = (self.bbox1, self.bbox2);
        let nb = if b2[2] - b2[0] < b1[2] - b1[0] {
            b2
        } else {
            b1
        };
        let (minlon2, maxlat2) = tile2lonlat(nb[0], nb[1], 32);
        let (maxlon2, minlat2) = tile2lonlat(nb[2], nb[3], 32);

        let mut merged = merge_layermaps(&[self.layermap.clone(), stats.layermap.clone()]);
        for v in merged.values_mut() {
            v.minzoom = self.minzoom;
            v.maxzoom = maxzoom;
        }

        make_metadata(
            name,
            self.minzoom,
            maxzoom,
            minlat,
            minlon,
            maxlat,
            maxlon,
            minlat2,
            minlon2,
            maxlat2,
            maxlon2,
            midlat,
            midlon,
            self.attribution.as_deref(),
            &merged,
            true,
            self.description.as_deref(),
            self.tilestats,
            &self.attribute_descriptions,
            "tippecanoe",
            &self.commandline,
            &stats.strategies,
            self.basezoom,
            self.droprate,
            self.retain_points_multiplier,
        )
    }
}
