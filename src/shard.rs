//! Builds split across machines. See DESIGN.md.
//!
//! `--shard I/N` makes part `I` of `N` of a tileset, and `manby merge` puts
//! the parts together. Every shard reads all of the input and orders it as a
//! whole run would, so that every choice tippecanoe makes from the whole input
//! (the zoom guesses, the drop rate, each feature's minimum zoom) comes out
//! the same. The tiles at and below the pivot zoom are split by their tile
//! at the pivot, in runs along the PMTiles Hilbert curve that each hold about
//! as many vertices: each shard reads only the tiles above its run, and makes
//! only the tiles in it. Shard 0 also makes the zooms above the pivot.
//!
//! A tile depends on its ancestors' features, on its zoom level's thresholds
//! and on the maximum zoom. The thresholds stay those of a whole run unless a
//! level at or below the pivot is made again with stricter ones, which
//! `--drop-densest-as-needed` and the like do when a tile is too big, and the
//! maximum zoom unless `--extend-zooms-if-still-dropping` extends it. Each
//! shard knows only of its own tiles there, so it records when either
//! happened, and `manby merge` says which zooms then differ from a whole
//! run's. With `--as-needed-per-tile`, no level is made again.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use rayon::prelude::*;
use tippecanoe::errors::EXIT_ARGS;
use tippecanoe::geometry::{DrawVec, VT_POINT, VT_POLYGON};
use tippecanoe::globals::MAX_ZOOM;
use tippecanoe::jsonpull::{JsonPull, JsonRef, JsonType};
use tippecanoe::mbtiles::{LayermapEntry, Tilestat};
use tippecanoe::options::A_HILBERT;
use tippecanoe::projection::{decode_hilbert, decode_quadkey};
use tippecanoe::serial::SerialVal;
use tippecanoe::tile::Strategy;

use crate::error::{Error, Result};
use crate::options::Options;
use crate::order::Plan;
use crate::store::FeatureStore;

/// The pivot zoom when `--shard-zoom` does not say: 4,096 tiles to split.
pub const DEFAULT_ZOOM: i32 = 6;

/// The metadata key of a shard's record.
pub const KEY: &str = "manby_shard";

/// `--shard I/N`: part `index` of `count`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shard {
    pub index: u32,
    pub count: u32,
}

impl Shard {
    /// `I/N`, with `I` from 0 to `N - 1`.
    pub fn parse(s: &str) -> Option<Shard> {
        let (i, n) = s.split_once('/')?;
        let index: u32 = i.trim().parse().ok()?;
        let count: u32 = n.trim().parse().ok()?;
        (count > 0 && index < count).then_some(Shard { index, count })
    }
}

/// The position of tile `z/x/y` along its zoom's Hilbert curve, from 0 to
/// `4^z - 1`. The four children of the tile at `d` are at `4d` to `4d + 3`,
/// so a run of tiles at one zoom is a run at every zoom below it.
pub fn hilbert(z: i32, x: u32, y: u32) -> u64 {
    tippecanoe::pmtiles::zxy_to_tileid(z as u8, x, y) - crate::sink::first_tile_id(z as u8)
}

/// The part of the tiles that one shard makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub index: u32,
    /// The pivot zoom.
    pub zoom: i32,
    /// The run of tiles at the pivot zoom, `lo..=hi` along the Hilbert
    /// curve; empty if `lo > hi`.
    pub lo: u64,
    pub hi: u64,
    /// The minimum zoom. Every shard reads every tile above it, since
    /// tippecanoe decides how many zooms it skips there from all of them.
    pub minzoom: i32,
}

impl Range {
    /// Whether the shard reads tile `z/x/y`: a tile whose run reaches its
    /// own, and with shard 0 every tile above the pivot.
    pub fn keeps(&self, z: i32, x: u32, y: u32) -> bool {
        if z < self.minzoom || (z < self.zoom && self.index == 0) {
            return true;
        }
        if self.lo > self.hi {
            return false;
        }
        if z < self.zoom {
            let dz = 2 * (self.zoom - z) as u32;
            let d = hilbert(z, x, y);
            let first = d << dz;
            let last = ((d + 1) << dz) - 1;
            return first <= self.hi && last >= self.lo;
        }
        let d = hilbert(z, x, y) >> (2 * (z - self.zoom) as u32);
        self.lo <= d && d <= self.hi
    }

    /// Whether the shard makes the tiles of zoom `z` that it reads.
    pub fn makes(&self, z: i32) -> bool {
        self.index == 0 || z >= self.zoom
    }
}

/// The pivot zoom of a run: `--shard-zoom`, or 6, kept from the first tile's
/// zoom and the minimum zoom down to the maximum zoom.
pub fn pivot(o: &Options, plan: &Plan) -> i32 {
    let low = plan.minzoom.max(plan.iz as i32);
    let high = plan.maxzoom.max(low);
    o.shard_zoom.unwrap_or(DEFAULT_ZOOM).clamp(low, high)
}

/// The runs of the pivot zoom's tiles for `count` shards, one after the
/// other along the Hilbert curve and together the whole zoom, each with
/// about as much work. A feature's work is its vertices, read again at each
/// zoom, and the tiles its bounding box covers at the maximum zoom, spread
/// over the pivot tiles its bounding box covers. Shard 0 also makes the
/// zooms above the pivot, which count as reading each vertex once more for
/// every two of them.
pub fn split(
    store: &FeatureStore,
    o: &Options,
    plan: &Plan,
    zoom: i32,
    count: u32,
) -> Vec<(u64, u64)> {
    let maxzoom = plan.maxzoom.max(zoom);
    let levels = (maxzoom - zoom + 1) as u64;
    let decode = if o.additional.get(A_HILBERT) {
        decode_hilbert
    } else {
        decode_quadkey
    };
    let features: &[crate::store::Feature] = &store.features;
    let (cells, vertices) = features
        .par_chunks(1 << 14)
        .map(|chunk| {
            let mut m: HashMap<u64, u64> = HashMap::new();
            let mut vertices = 0u64;
            let mut g: DrawVec = Vec::new();
            for f in chunk {
                let n = f.geom_len as u64;
                vertices += n;
                g.clear();
                store
                    .geometry
                    .decode(f.geom_start, f.geom_len, store.geometry_scale, &mut g);
                let bbox = g.iter().fold(None, |b: Option<[i64; 4]>, d| {
                    let (x, y) = (d.x.clamp(0, WORLD), d.y.clamp(0, WORLD));
                    Some(match b {
                        None => [x, y, x, y],
                        Some(b) => [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)],
                    })
                });
                let Some(b) = bbox.filter(|_| f.t as i32 != VT_POINT) else {
                    // a point, or no geometry: one tile, where it is
                    let (wx, wy) = decode(f.ix);
                    let at = hilbert(zoom, cell_of(wx as i64, zoom), cell_of(wy as i64, zoom));
                    *m.entry(at).or_default() += n * levels + TILE_WEIGHT;
                    continue;
                };
                let (cx0, cy0, cx1, cy1) = (
                    cell_of(b[0], zoom),
                    cell_of(b[1], zoom),
                    cell_of(b[2], zoom),
                    cell_of(b[3], zoom),
                );
                let cells = (cx1 - cx0 + 1) as u64 * (cy1 - cy0 + 1) as u64;
                // the tiles at the maximum zoom in each pivot tile: those of
                // the bounding box for a polygon, of its edges for a line
                let dz = (maxzoom - zoom) as u32;
                let (tx0, ty0, tx1, ty1) = (
                    cell_of(b[0], maxzoom) as u64,
                    cell_of(b[1], maxzoom) as u64,
                    cell_of(b[2], maxzoom) as u64,
                    cell_of(b[3], maxzoom) as u64,
                );
                let polygon = f.t as i32 == VT_POLYGON;
                for cx in cx0..=cx1 {
                    for cy in cy0..=cy1 {
                        let span = |lo: u64, hi: u64, c: u32| {
                            let first = (c as u64) << dz;
                            let last = first + (1u64 << dz) - 1;
                            hi.min(last) - lo.max(first) + 1
                        };
                        let (w, h) = (span(tx0, tx1, cx), span(ty0, ty1, cy));
                        let tiles = if polygon { w * h } else { w.max(h) };
                        let weight = n * levels / cells + tiles * TILE_WEIGHT;
                        *m.entry(hilbert(zoom, cx, cy)).or_default() += weight;
                    }
                }
            }
            (m, vertices)
        })
        .reduce(
            || (HashMap::new(), 0),
            |(mut a, va), (b, vb)| {
                for (k, v) in b {
                    *a.entry(k).or_default() += v;
                }
                (a, va + vb)
            },
        );
    let mut cells: Vec<(u64, u64)> = cells.into_iter().collect();
    cells.sort_unstable();
    let above = (vertices as f64 * zoom as f64 * LOW_WEIGHT) as u64;
    cut(&cells, zoom, count, above)
}

/// The work of a tile at the maximum zoom, in vertices read once.
const TILE_WEIGHT: u64 = 64;
/// The work of making a zoom above the pivot, in vertices read once for each
/// vertex.
const LOW_WEIGHT: f64 = 0.5;
/// The largest world coordinate.
const WORLD: i64 = (1i64 << 32) - 1;

/// The column or row at `zoom` of world coordinate `c`.
fn cell_of(c: i64, zoom: i32) -> u32 {
    if zoom == 0 {
        0
    } else {
        (c.clamp(0, WORLD) >> (32 - zoom)) as u32
    }
}

/// Splits the weighted cells (in Hilbert order) into `count` runs, the first
/// with `above` less than the others for the zooms above the pivot: each run
/// after the first starts at the first cell with at least its share of the
/// weight before it. An empty run has `lo > hi`.
fn cut(cells: &[(u64, u64)], zoom: i32, count: u32, above: u64) -> Vec<(u64, u64)> {
    let end = 1u64 << (2 * zoom as u32);
    let total: u128 = cells.iter().map(|&(_, w)| w as u128).sum::<u128>() + above as u128;
    let mut starts = vec![0u64];
    let (mut before, mut cell) = (above as u128, 0);
    for i in 1..count as u128 {
        let target = total * i / count as u128;
        while cell < cells.len() && before < target {
            before += cells[cell].1 as u128;
            cell += 1;
        }
        let start = cells.get(cell).map_or(end, |c| c.0);
        starts.push(start.max(*starts.last().unwrap()));
    }
    starts.push(end);
    starts
        .windows(2)
        .map(|w| {
            if w[0] < w[1] {
                (w[0], w[1] - 1)
            } else {
                (1, 0)
            }
        })
        .collect()
}

/// What a shard records in its tileset's metadata for `manby merge`: what
/// the whole run's metadata is made of, and what its own tiling found.
#[derive(Clone, Debug, Default)]
pub struct Record {
    pub index: u32,
    pub count: u32,
    pub zoom: i32,
    pub ranges: Vec<(u64, u64)>,
    /// The same for every shard of one run.
    pub fingerprint: u64,
    /// `-n`, if given; otherwise the tileset is named after its path.
    pub name: Option<String>,
    pub meta: crate::MetaInputs,
    pub tiling: crate::tiler::ShardTiling,
}

impl Record {
    pub fn to_json(&self) -> String {
        let mut s = String::from("{");
        let _ = write!(
            s,
            "\"version\":1,\"index\":{},\"count\":{},\"zoom\":{},\"ranges\":[",
            self.index, self.count, self.zoom
        );
        for (i, (lo, hi)) in self.ranges.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            let _ = write!(s, "[\"{}\",\"{}\"]", lo, hi);
        }
        let _ = write!(s, "],\"fingerprint\":\"{}\"", self.fingerprint);
        s.push_str(",\"name\":");
        match &self.name {
            Some(n) => string(&mut s, n),
            None => s.push_str("null"),
        }
        s.push_str(&meta_json(&self.meta));
        let t = &self.tiling;
        let _ = write!(
            s,
            ",\"tiles\":{},\"maxzoom\":{},\"largest_written\":{},\"incomplete\":{},\"incomplete_maxzoom\":{}",
            t.tiles, t.maxzoom, t.largest_written, t.incomplete, t.incomplete_maxzoom
        );
        s.push_str(",\"strategies\":[");
        for (i, (z, sz, st)) in t.strategies.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            let _ = write!(
                s,
                "[{},{},{},{},{},{},{},{},{},{},{}]",
                z,
                sz,
                st.dropped_by_rate,
                st.dropped_by_gamma,
                st.dropped_as_needed,
                st.coalesced_as_needed,
                st.detail_reduced,
                st.tiny_polygons,
                st.truncated_zooms,
                st.tile_size,
                st.feature_count
            );
        }
        s.push_str("],\"tops\":[");
        for (i, (z, len, x, y)) in t.tops.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            let _ = write!(s, "[{},\"{}\",{},{}]", z, len, x, y);
        }
        s.push_str("],\"layermaps\":[");
        for (i, (z, map)) in t.layermaps.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            let _ = write!(s, "[{},", z);
            layermap_json(&mut s, map);
            s.push(']');
        }
        s.push_str("],\"again\":[");
        for (i, z) in t.again.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            let _ = write!(s, "{}", z);
        }
        s.push_str("]}");
        s
    }

    pub fn from_json(text: &str) -> std::result::Result<Record, String> {
        let tree = JsonPull::begin_string(text.as_bytes())
            .read_tree()
            .ok_or("not JSON")?;
        let r = tree.root();
        let bad = |what: &str| format!("bad {}", what);
        if get_u64(r, "version") != Some(1) {
            return Err("from another version of manby".to_string());
        }
        let ranges = r
            .hash_get("ranges")
            .filter(|v| v.type_() == JsonType::Array)
            .ok_or_else(|| bad("ranges"))?
            .array_iter()
            .map(|p| Some((u64_of(p.array_get(0))?, u64_of(p.array_get(1))?)))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| bad("ranges"))?;
        let bbox = |k: &str| -> std::result::Result<[i64; 4], String> {
            let v = r
                .hash_get(k)
                .filter(|v| v.type_() == JsonType::Array && v.array_len() == 4)
                .ok_or_else(|| bad(k))?;
            let mut b = [0i64; 4];
            for (i, b) in b.iter_mut().enumerate() {
                *b = i64_of(v.array_get(i)).ok_or_else(|| bad(k))?;
            }
            Ok(b)
        };
        let opt_string = |k: &str| {
            r.hash_get(k)
                .filter(|v| v.type_() == JsonType::String)
                .map(|v| v.string().to_string())
        };
        let mut attribute_descriptions = BTreeMap::new();
        if let Some(h) = r.hash_get("attribute_descriptions") {
            for (k, v) in h.entries_iter() {
                attribute_descriptions.insert(k.string().to_string(), v.string().to_string());
            }
        }
        let num = |k: &str| get_i64(r, k).ok_or_else(|| bad(k));
        let meta = crate::MetaInputs {
            minzoom: num("minzoom")? as i32,
            bbox: bbox("bbox")?,
            bbox1: bbox("bbox1")?,
            bbox2: bbox("bbox2")?,
            attribution: opt_string("attribution"),
            description: opt_string("description"),
            tilestats: get_bool(r, "tilestats").ok_or_else(|| bad("tilestats"))?,
            attribute_descriptions,
            commandline: opt_string("commandline").ok_or_else(|| bad("commandline"))?,
            basezoom: num("basezoom")? as i32,
            droprate: f64::from_bits(get_u64(r, "droprate").ok_or_else(|| bad("droprate"))?),
            retain_points_multiplier: num("retain_points_multiplier")? as i32,
            layermap: r
                .hash_get("layermap")
                .and_then(layermap_of)
                .ok_or_else(|| bad("layermap"))?,
            tile_compression: get_bool(r, "tile_compression")
                .ok_or_else(|| bad("tile_compression"))?,
            tile_format: opt_string("tile_format")
                .and_then(|f| crate::mlt::TileFormat::parse(&f))
                .ok_or_else(|| bad("tile_format"))?,
        };
        let array = |k: &str| {
            r.hash_get(k)
                .filter(|v| v.type_() == JsonType::Array)
                .map(|v| v.array())
                .ok_or_else(|| bad(k))
        };
        let mut strategies = Vec::new();
        for a in array("strategies")? {
            let n: Vec<u64> = a
                .array_iter()
                .map(u64_of)
                .collect::<Option<_>>()
                .filter(|n: &Vec<u64>| n.len() == 11)
                .ok_or_else(|| bad("strategies"))?;
            let u = |i: usize| n[i] as usize;
            strategies.push((
                n[0] as i32,
                n[1] as i32,
                Strategy {
                    dropped_by_rate: u(2),
                    dropped_by_gamma: u(3),
                    dropped_as_needed: u(4),
                    coalesced_as_needed: u(5),
                    detail_reduced: u(6),
                    tiny_polygons: u(7),
                    truncated_zooms: u(8),
                    tile_size: u(9),
                    feature_count: u(10),
                },
            ));
        }
        let mut tops = Vec::new();
        for a in array("tops")? {
            let top = (|| {
                Some((
                    i64_of(a.array_get(0))? as i32,
                    i64_of(a.array_get(1))?,
                    u64_of(a.array_get(2))? as u32,
                    u64_of(a.array_get(3))? as u32,
                ))
            })()
            .ok_or_else(|| bad("tops"))?;
            tops.push(top);
        }
        let mut layermaps = Vec::new();
        for a in array("layermaps")? {
            let z = i64_of(a.array_get(0)).ok_or_else(|| bad("layermaps"))? as i32;
            let map = layermap_of(a.array_get(1)).ok_or_else(|| bad("layermaps"))?;
            layermaps.push((z, map));
        }
        let again = array("again")?
            .into_iter()
            .map(|z| i64_of(z).map(|z| z as i32))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| bad("again"))?;
        let tiling = crate::tiler::ShardTiling {
            tiles: num("tiles")? as u64,
            maxzoom: num("maxzoom")? as i32,
            largest_written: num("largest_written")? as i32,
            incomplete: get_bool(r, "incomplete").ok_or_else(|| bad("incomplete"))?,
            incomplete_maxzoom: num("incomplete_maxzoom")? as i32,
            strategies,
            tops,
            layermaps,
            again,
        };
        Ok(Record {
            index: num("index")? as u32,
            count: num("count")? as u32,
            zoom: num("zoom")? as i32,
            ranges,
            fingerprint: get_u64(r, "fingerprint").ok_or_else(|| bad("fingerprint"))?,
            name: opt_string("name"),
            meta,
            tiling,
        })
    }
}

/// The tile `z/x/y` is this shard's to make.
impl Range {
    pub fn owns(&self, z: i32, x: u32, y: u32) -> bool {
        if z < self.zoom {
            self.index == 0
        } else {
            self.keeps(z, x, y)
        }
    }
}

/// Tells that some zooms of a shard were made with thresholds or a maximum
/// zoom of its own.
pub(crate) fn again_note(s: Shard, zooms: &[i32]) -> String {
    let mut zooms = zooms.to_vec();
    zooms.sort_unstable();
    let zooms = &zooms[..];
    format!(
        "shard {}/{}: {} {} made again with stricter thresholds, or extended the maximum zoom, for this shard's tiles alone, so {} can differ from a whole run's; --as-needed-per-tile keeps each tile's thresholds to itself",
        s.index,
        s.count,
        if zooms.len() == 1 { "zoom" } else { "zooms" },
        list(zooms),
        if zooms.len() == 1 { "it" } else { "they" }
    )
}

fn list(zooms: &[i32]) -> String {
    zooms
        .iter()
        .map(|z| z.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The same for every shard of one run, and different for another run.
pub(crate) fn fingerprint(
    m: &crate::MetaInputs,
    plan: &Plan,
    store: &FeatureStore,
    zoom: i32,
    ranges: &[(u64, u64)],
) -> u64 {
    let mut m = m.clone();
    // the output's path differs from shard to shard
    m.commandline.clear();
    let text = format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{:?}",
        env!("CARGO_PKG_VERSION"),
        meta_json(&m),
        plan.maxzoom,
        plan.iz,
        plan.ix,
        plan.iy,
        store.features.len(),
        zoom,
        ranges
    );
    xxhash_rust::xxh3::xxh3_64(text.as_bytes())
}

/// What `manby merge` does: puts the shards of one run together.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct MergeOptions {
    /// The shards' tilesets, one of each.
    pub inputs: Vec<String>,
    /// `-o` (PMTiles or MBTiles) or `-e` (a directory).
    pub output: crate::options::Output,
    /// `-f`: replace an existing tileset.
    pub force: bool,
    /// `-q`: no notes.
    pub quiet: bool,
    /// `argv[0]`, for messages.
    pub program: String,
}

/// What a merge made.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct MergeSummary {
    pub tiles: u64,
    pub minzoom: i32,
    pub maxzoom: i32,
    /// As with a whole run: a tile could not be made small enough.
    pub incomplete: bool,
    /// The zooms whose tiles differ from a whole run's.
    pub differ: Vec<i32>,
}

const MERGE_USAGE: &str = "Usage: manby merge -o out.pmtiles [-f] [-q] shard.pmtiles ...\n\
Puts the tilesets made with manby --shard I/N, one of each I, together as the run without --shard makes them.\n\
  -o, --output=FILE                 PMTiles or MBTiles to write\n\
  -e, --output-to-directory=DIR     a directory of tiles to write\n\
  -f, --force                       replace an existing tileset\n\
  -q, --quiet                       no notes";

/// `manby merge`'s arguments, after the program and subcommand in `argv[0]`.
pub fn parse_args(argv: &[String]) -> Result<MergeOptions> {
    let mut o = MergeOptions {
        program: argv
            .first()
            .cloned()
            .unwrap_or_else(|| "manby merge".to_string()),
        ..Default::default()
    };
    let usage = |status: i32| Error::new(status, MERGE_USAGE);
    let mut args = argv.iter().skip(1);
    while let Some(a) = args.next() {
        let (key, inline) = match a.split_once('=') {
            Some((k, v)) if k.starts_with("--") => (k, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let mut value = || inline.clone().or_else(|| args.next().cloned());
        match key {
            "-h" | "--help" => return Err(usage(tippecanoe::errors::EXIT_SUCCESS)),
            "-f" | "--force" => o.force = true,
            "-q" | "--quiet" => o.quiet = true,
            "-o" | "--output" => {
                let out = value().ok_or_else(|| usage(EXIT_ARGS))?;
                o.output = if tippecanoe::pmtiles_file::pmtiles_has_suffix(&out) {
                    crate::options::Output::Pmtiles(out)
                } else {
                    crate::options::Output::Mbtiles(out)
                };
            }
            "-e" | "--output-to-directory" => {
                o.output =
                    crate::options::Output::Directory(value().ok_or_else(|| usage(EXIT_ARGS))?);
            }
            _ if key.starts_with('-') && key.len() > 1 => return Err(usage(EXIT_ARGS)),
            _ => o.inputs.push(a.clone()),
        }
    }
    if o.inputs.is_empty() || o.output.path().is_none() {
        return Err(usage(EXIT_ARGS));
    }
    Ok(o)
}

/// Puts the shards of one run together: their tiles, and the metadata the
/// whole run would have written.
pub fn merge(o: &MergeOptions) -> Result<MergeSummary> {
    use tippecanoe::errors::{EXIT_OPEN, EXIT_READ};
    let open = |m: String| Error::new(EXIT_OPEN, m);

    let mut parts = Vec::new();
    for path in &o.inputs {
        let ts = crate::serve::tileset::Tileset::new(path, String::new()).map_err(open)?;
        let opened = ts.opened().map_err(open)?;
        let text = opened.meta.shard.clone().ok_or_else(|| {
            Error::new(
                EXIT_ARGS,
                format!("{}: {}: not a shard, made with --shard", o.program, path),
            )
        })?;
        let record = Record::from_json(&text)
            .map_err(|e| Error::new(EXIT_ARGS, format!("{}: {}: {}", o.program, path, e)))?;
        parts.push((path.clone(), opened, record));
    }

    let first = parts[0].2.clone();
    for (path, _, r) in &parts {
        if r.count != first.count
            || r.zoom != first.zoom
            || r.ranges != first.ranges
            || r.fingerprint != first.fingerprint
        {
            return Err(Error::new(
                EXIT_ARGS,
                format!(
                    "{}: {} is a shard of another run than {}: the shards of a run have the same input and options",
                    o.program, path, parts[0].0
                ),
            ));
        }
    }
    parts.sort_by_key(|p| p.2.index);
    for (i, (path, _, r)) in parts.iter().enumerate() {
        if i > 0 && parts[i - 1].2.index == r.index {
            return Err(Error::new(
                EXIT_ARGS,
                format!(
                    "{}: {} and {} are both shard {}/{}",
                    o.program,
                    parts[i - 1].0,
                    path,
                    r.index,
                    r.count
                ),
            ));
        }
    }
    let missing: Vec<String> = (0..first.count)
        .filter(|i| parts.iter().all(|p| p.2.index != *i))
        .map(|i| format!("{}/{}", i, first.count))
        .collect();
    if !missing.is_empty() {
        return Err(Error::new(
            EXIT_ARGS,
            format!(
                "{}: missing {} {}",
                o.program,
                if missing.len() == 1 {
                    "shard"
                } else {
                    "shards"
                },
                missing.join(", ")
            ),
        ));
    }

    let records: Vec<Record> = parts.iter().map(|p| p.2.clone()).collect();
    let (mut stats, limit, differ) = combine(&records);
    let meta = &first.meta;

    let sink = crate::sink::open(
        &o.output,
        &crate::sink::SinkSettings {
            force: o.force,
            tile_compression: meta.tile_compression,
            tile_format: meta.tile_format,
            program: o.program.clone(),
            ..Default::default()
        },
    )?;
    for (path, opened, r) in &parts {
        let range = Range {
            index: r.index,
            zoom: r.zoom,
            lo: r.ranges[r.index as usize].0,
            hi: r.ranges[r.index as usize].1,
            minzoom: meta.minzoom,
        };
        let read = |e: String| Error::new(EXIT_READ, format!("{}: {}", path, e));
        for t in opened.list().map_err(read)? {
            if limit.is_some_and(|l| t.z as i32 > l) {
                continue;
            }
            let data = opened.data(&t).map_err(read)?;
            for i in 0..t.count {
                let (z, x, y) = if t.count == 1 {
                    (t.z, t.x, t.y)
                } else {
                    let c = tippecanoe::pmtiles::tileid_to_zxy(t.id + i);
                    (c.z as u8, c.x as u32, c.y as u32)
                };
                if !range.owns(z as i32, x, y) {
                    return Err(Error::new(
                        EXIT_ARGS,
                        format!(
                            "{}: {}: tile {}/{}/{} is not shard {}/{}'s to make",
                            o.program, path, z, x, y, r.index, r.count
                        ),
                    ));
                }
                sink.write_tile(z, x, y, &data)?;
                stats.tiles += 1;
            }
        }
    }

    let name = first
        .name
        .clone()
        .unwrap_or_else(|| o.output.path().unwrap_or_default().to_string());
    let metadata = meta.metadata(&name, &stats);
    sink.finish(&metadata)?;

    if stats.incomplete && !o.quiet {
        eprintln!(
            "\n\n\n*** NOTE TILES ONLY COMPLETE THROUGH ZOOM {} ***",
            stats.maxzoom
        );
    }
    if !differ.is_empty() && !o.quiet {
        eprintln!(
            "{}: {} {} can differ from a whole run's: a shard made {} again with stricter thresholds, or extended the maximum zoom, which a whole run does for every tile of the zoom; --as-needed-per-tile keeps each tile's thresholds to itself",
            o.program,
            if differ.len() == 1 { "zoom" } else { "zooms" },
            list(&differ),
            if differ.len() == 1 { "it" } else { "them" }
        );
    }
    Ok(MergeSummary {
        tiles: stats.tiles,
        minzoom: meta.minzoom,
        maxzoom: stats.maxzoom,
        incomplete: stats.incomplete,
        differ,
    })
}

/// What the whole run's tiling found, from what each shard found of the
/// zoom levels it speaks for: the zooms above the pivot for shard 0, and its
/// own tiles below. Also the last zoom whose tiles the whole run writes, if
/// it stopped early, and the zooms that differ from a whole run's.
fn combine(records: &[Record]) -> (crate::tiler::TileStats, Option<i32>, Vec<i32>) {
    let pivot = records[0].zoom;
    // the level where a whole run stops: where the first shard did
    let limit = records
        .iter()
        .filter(|r| r.tiling.incomplete)
        .map(|r| r.tiling.incomplete_maxzoom + 1)
        .min();
    let speaks = |r: &Record, z: i32| (r.index == 0 || z >= pivot) && limit.is_none_or(|l| z <= l);

    let mut strategies: Vec<Strategy> = Vec::new();
    for r in records {
        let mut own: Vec<Strategy> = Vec::new();
        for (z, sz, st) in &r.tiling.strategies {
            if !speaks(r, *z) {
                continue;
            }
            let sz = *sz as usize;
            if sz >= own.len() {
                own.resize(sz + 1, Strategy::default());
            }
            own[sz] = *st;
        }
        if own.len() > strategies.len() {
            strategies.resize(own.len(), Strategy::default());
        }
        for (a, b) in strategies.iter_mut().zip(&own) {
            a.dropped_by_rate += b.dropped_by_rate;
            a.dropped_by_gamma += b.dropped_by_gamma;
            a.dropped_as_needed += b.dropped_as_needed;
            a.coalesced_as_needed += b.coalesced_as_needed;
            a.detail_reduced += b.detail_reduced;
            a.tiny_polygons += b.tiny_polygons;
            a.truncated_zooms += b.truncated_zooms;
            a.tile_size = a.tile_size.max(b.tile_size);
            a.feature_count = a.feature_count.max(b.feature_count);
        }
    }

    // each level's tile with the most vertices, of all the shards'
    let mut tops: BTreeMap<i32, (i64, u32, u32)> = BTreeMap::new();
    for r in records {
        for &(z, len, x, y) in &r.tiling.tops {
            if !speaks(r, z) {
                continue;
            }
            let t = tops.entry(z).or_insert((len, x, y));
            if len > t.0 || (len == t.0 && (x, y) < (t.1, t.2)) {
                *t = (len, x, y);
            }
        }
    }
    // as the levels are finished, in zoom order
    let (mut midx, mut midy) = (0u32, 0u32);
    for (len, x, y) in tops.into_values() {
        let a: u64 = ((x as u64) << 32) | y as u64;
        let b: u64 = ((midx as u64) << 32) | midy as u64;
        if len > 0 || (len == 0 && a < b) {
            (midx, midy) = (x, y);
        }
    }

    let maps: Vec<BTreeMap<String, LayermapEntry>> = records
        .iter()
        .flat_map(|r| {
            r.tiling
                .layermaps
                .iter()
                .filter(|(z, _)| speaks(r, *z))
                .map(|(_, m)| m.clone())
        })
        .collect();

    let planned = records.iter().map(|r| r.tiling.maxzoom).max().unwrap_or(0);
    let largest_written = records
        .iter()
        .map(|r| r.tiling.largest_written)
        .max()
        .unwrap_or(-1);
    let incomplete = limit.is_some();
    if !incomplete && strategies.len() < (planned + 1) as usize {
        strategies.resize((planned + 1) as usize, Strategy::default());
    }
    let maxzoom = match limit {
        Some(l) => l - 1,
        None if largest_written >= 0 && planned > largest_written => largest_written,
        None => planned,
    };

    let mut differ: Vec<i32> = records
        .iter()
        .flat_map(|r| r.tiling.again.iter().copied())
        .filter(|&z| limit.is_none_or(|l| z <= l))
        .collect();
    if records.iter().any(|r| r.tiling.maxzoom != planned) {
        let low = records
            .iter()
            .map(|r| r.tiling.maxzoom)
            .min()
            .unwrap_or(planned);
        differ.extend(low + 1..=planned);
    }
    differ.sort_unstable();
    differ.dedup();

    let stats = crate::tiler::TileStats {
        tiles: 0,
        strategies,
        maxzoom,
        midx,
        midy,
        layermap: crate::tiler::merge_tilestats(maps),
        incomplete,
        shard: None,
    };
    (stats, limit, differ)
}

/// The record's fields from what the metadata is made of, each after a comma.
fn meta_json(m: &crate::MetaInputs) -> String {
    let mut s = String::new();
    let _ = write!(s, ",\"minzoom\":{}", m.minzoom);
    for (k, b) in [("bbox", m.bbox), ("bbox1", m.bbox1), ("bbox2", m.bbox2)] {
        let _ = write!(
            s,
            ",\"{}\":[\"{}\",\"{}\",\"{}\",\"{}\"]",
            k, b[0], b[1], b[2], b[3]
        );
    }
    for (k, v) in [
        ("attribution", &m.attribution),
        ("description", &m.description),
    ] {
        let _ = write!(s, ",\"{}\":", k);
        match v {
            Some(v) => string(&mut s, v),
            None => s.push_str("null"),
        }
    }
    let _ = write!(s, ",\"tilestats\":{}", m.tilestats);
    s.push_str(",\"attribute_descriptions\":{");
    for (i, (k, v)) in m.attribute_descriptions.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        string(&mut s, k);
        s.push(':');
        string(&mut s, v);
    }
    s.push_str("},\"commandline\":");
    string(&mut s, &m.commandline);
    let _ = write!(
        s,
        ",\"basezoom\":{},\"droprate\":\"{}\",\"retain_points_multiplier\":{}",
        m.basezoom,
        m.droprate.to_bits(),
        m.retain_points_multiplier
    );
    s.push_str(",\"layermap\":");
    layermap_json(&mut s, &m.layermap);
    let _ = write!(
        s,
        ",\"tile_compression\":{},\"tile_format\":\"{}\"",
        m.tile_compression,
        m.tile_format.metadata_format()
    );

    s
}

/// `s` as a JSON string.
fn string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn layermap_json(out: &mut String, map: &BTreeMap<String, LayermapEntry>) {
    out.push('{');
    for (i, (name, e)) in map.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        string(out, name);
        let _ = write!(
            out,
            ":{{\"id\":{},\"minzoom\":{},\"maxzoom\":{},\"points\":{},\"lines\":{},\"polygons\":{},\"retain\":{},\"description\":",
            e.id, e.minzoom, e.maxzoom, e.points, e.lines, e.polygons, e.retain
        );
        string(out, &e.description);
        out.push_str(",\"tilestats\":{");
        for (j, (attr, ts)) in e.tilestats.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            string(out, attr);
            let _ = write!(
                out,
                ":{{\"type\":{},\"min\":\"{}\",\"max\":\"{}\",\"samples\":[",
                ts.type_,
                ts.min.to_bits(),
                ts.max.to_bits()
            );
            for (k, v) in ts.sample_values.iter().enumerate() {
                if k > 0 {
                    out.push(',');
                }
                let _ = write!(out, "[{},", v.type_);
                string(out, &v.s);
                out.push(']');
            }
            out.push_str("]}");
        }
        out.push_str("}}");
    }
    out.push('}');
}

fn layermap_of(v: JsonRef<'_>) -> Option<BTreeMap<String, LayermapEntry>> {
    if v.type_() != JsonType::Hash {
        return None;
    }
    let mut map = BTreeMap::new();
    for (name, e) in v.entries_iter() {
        let n = |k: &str| get_u64(e, k).map(|n| n as usize);
        let mut tilestats = BTreeMap::new();
        for (attr, ts) in e.hash_get("tilestats")?.entries_iter() {
            let mut sample_values = Vec::new();
            for sv in ts.hash_get("samples")?.array_iter() {
                sample_values.push(SerialVal {
                    type_: i64_of(sv.array_get(0))? as i32,
                    s: sv.array_get(1).string().to_string(),
                });
            }
            tilestats.insert(
                attr.string().to_string(),
                Tilestat {
                    sample_values,
                    min: f64::from_bits(get_u64(ts, "min")?),
                    max: f64::from_bits(get_u64(ts, "max")?),
                    type_: get_i64(ts, "type")? as i32,
                },
            );
        }
        map.insert(
            name.string().to_string(),
            LayermapEntry {
                id: n("id")?,
                tilestats,
                minzoom: get_i64(e, "minzoom")? as i32,
                maxzoom: get_i64(e, "maxzoom")? as i32,
                description: e.hash_get("description")?.string().to_string(),
                points: n("points")?,
                lines: n("lines")?,
                polygons: n("polygons")?,
                retain: n("retain")?,
            },
        );
    }
    Some(map)
}

/// A whole number, written as a number or, past what a double holds
/// exactly, as a string.
fn u64_of(v: JsonRef<'_>) -> Option<u64> {
    match v.type_() {
        JsonType::String => v.string().parse().ok(),
        JsonType::Number => Some(v.number() as u64),
        _ => None,
    }
}

fn i64_of(v: JsonRef<'_>) -> Option<i64> {
    match v.type_() {
        JsonType::String => v.string().parse().ok(),
        JsonType::Number => Some(v.number() as i64),
        _ => None,
    }
}

fn get_u64(v: JsonRef<'_>, k: &str) -> Option<u64> {
    v.hash_get(k).and_then(u64_of)
}

fn get_i64(v: JsonRef<'_>, k: &str) -> Option<i64> {
    v.hash_get(k).and_then(i64_of)
}

fn get_bool(v: JsonRef<'_>, k: &str) -> Option<bool> {
    match v.hash_get(k)?.type_() {
        JsonType::True => Some(true),
        JsonType::False => Some(false),
        _ => None,
    }
}

/// The checks `--shard` makes of the other options.
pub fn check_options(o: &Options) -> Result<()> {
    let Some(_) = o.shard else {
        if o.shard_zoom.is_some() {
            return Err(Error::new(
                EXIT_ARGS,
                format!("{}: --shard-zoom needs --shard", o.program),
            ));
        }
        return Ok(());
    };
    if o.additional
        .get(tippecanoe::options::A_VARIABLE_DEPTH_PYRAMID)
    {
        return Err(Error::new(
            EXIT_ARGS,
            format!(
                "{}: --shard does not work with --generate-variable-depth-tile-pyramid",
                o.program
            ),
        ));
    }
    if let Some(z) = o.shard_zoom
        && !(0..=MAX_ZOOM).contains(&z)
    {
        return Err(Error::new(
            EXIT_ARGS,
            format!(
                "{}: --shard-zoom must be from 0 to {}, not {}",
                o.program, MAX_ZOOM, z
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_shards() {
        assert_eq!(Shard::parse("0/1"), Some(Shard { index: 0, count: 1 }));
        assert_eq!(Shard::parse("3/4"), Some(Shard { index: 3, count: 4 }));
        assert_eq!(Shard::parse("4/4"), None);
        assert_eq!(Shard::parse("0/0"), None);
        assert_eq!(Shard::parse("1"), None);
        assert_eq!(Shard::parse("-1/2"), None);
    }

    #[test]
    fn children_follow_their_parent_on_the_curve() {
        for z in 0..6 {
            for x in 0..1u32 << z {
                for y in 0..1u32 << z {
                    let d = hilbert(z, x, y);
                    for (cx, cy) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                        let c = hilbert(z + 1, 2 * x + cx, 2 * y + cy);
                        assert_eq!(c >> 2, d, "{}/{}/{}", z, x, y);
                    }
                }
            }
        }
    }

    #[test]
    fn cuts_cover_the_zoom() {
        let cells = [(0, 10), (1, 10), (5, 10), (9, 10), (12, 10), (15, 10)];
        assert_eq!(cut(&cells, 2, 3, 0), vec![(0, 4), (5, 11), (12, 15)]);
        assert_eq!(cut(&[], 0, 1, 0), vec![(0, 0)]);
        // more shards than cells, and a first cell at 0: some runs are empty
        for cells in [vec![(7, 1)], vec![(0, 1), (3, 100)], vec![]] {
            let r = cut(&cells, 2, 5, 3);
            assert_eq!(r.len(), 5);
            for d in 0..16 {
                let n = r.iter().filter(|&&(lo, hi)| lo <= d && d <= hi).count();
                assert_eq!(n, 1, "{:?}: {}", r, d);
            }
        }
    }

    #[test]
    fn every_tile_has_one_shard() {
        let ranges = cut(&[(1, 5), (2, 5), (7, 5), (40, 5), (41, 5)], 3, 4, 0);
        for z in 0..6 {
            for x in 0..1u32 << z {
                for y in 0..1u32 << z {
                    let owners: Vec<u32> = (0..4)
                        .filter(|&i| {
                            let r = Range {
                                index: i,
                                zoom: 3,
                                lo: ranges[i as usize].0,
                                hi: ranges[i as usize].1,
                                minzoom: 0,
                            };
                            r.keeps(z, x, y) && r.makes(z)
                        })
                        .collect();
                    assert_eq!(owners.len(), 1, "{}/{}/{}: {:?}", z, x, y, owners);
                    if z >= 3 {
                        // and every shard that makes a tile reads its parent
                        let i = owners[0];
                        let r = Range {
                            index: i,
                            zoom: 3,
                            lo: ranges[i as usize].0,
                            hi: ranges[i as usize].1,
                            minzoom: 0,
                        };
                        assert!(r.keeps(z - 1, x / 2, y / 2));
                    }
                }
            }
        }
    }
}
