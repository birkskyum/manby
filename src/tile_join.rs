//! Port of tile-join.cpp, the `tile-join` program.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::ffi::{CStr, c_char};
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Mutex, RwLock};

use memmap2::Mmap;
use rusqlite::{Connection, OpenFlags, ffi};

use crate::clip::overzoom_source_tiles;
use crate::cstd::{atof, atoi, strerror, strtod};
use crate::csv::{csv_dequote, is_number, readcsv};
use crate::dirtiles::{
    Zxy, check_dir, dir_read_tile, dir_write_metadata, dir_write_tile, dirmeta2tmp,
    enumerate_dirtiles,
};
use crate::errors::{
    EXIT_ARGS, EXIT_CLOSE, EXIT_IMPOSSIBLE, EXIT_MEMORY, EXIT_MVT, EXIT_PROTOBUF, EXIT_SQLITE,
};
use crate::evaluator::{evaluate_feature, parse_filter, read_filter};
use crate::geometry::SourceTile;
use crate::getopt::{Getopt, LongOption, NO_ARGUMENT, REQUIRED_ARGUMENT};
use crate::globals::{
    CPUS, MAX_TILESTATS_ATTRIBUTES, MAX_TILESTATS_SAMPLE_VALUES, MAX_TILESTATS_VALUES,
    PROGRESS_TIME_ENABLED, QUIET,
};
use crate::jsonpull::{JsonPull, JsonRef, JsonTree, JsonType};
use crate::mbtiles::{
    LayermapEntry, add_to_tilestats, make_metadata, mbtiles_close, mbtiles_open,
    mbtiles_write_metadata, mbtiles_write_tile, merge_layermaps,
};
use crate::mvt::{
    MVT_DOUBLE, MVT_LINESTRING, MVT_LINETO, MVT_MOVETO, MVT_NULL, MVT_POINT, MVT_POLYGON,
    MVT_STRING, MvtFeature, MvtLayer, MvtTile, MvtValue, compress, mvt_value_to_serial_val,
    stringified_to_mvt_value,
};
use crate::platform::get_num_avail_cpus;
use crate::pmtiles::EntryZxy;
use crate::pmtiles_file::{
    check_pmtiles, mbtiles_map_image_to_pmtiles, pmtiles_entries_tms, pmtiles_get_tile,
    pmtiles_has_suffix, pmtilesmeta2tmp,
};
use crate::projection::tile2lonlat;
use crate::serial::SerialVal;
use crate::text::{format_commandline, read_unidecode};
use crate::thread::thread_create;
use crate::tile::Strategy;
use crate::tile_cache::TileCache;
use crate::usage::{UsageRequiredOption, getopt_string, print_usage, strip_usage_headings};

static PK: AtomicI32 = AtomicI32::new(0);
static PC: AtomicI32 = AtomicI32::new(0);
static PG: AtomicI32 = AtomicI32::new(0);
static PE: AtomicI32 = AtomicI32::new(0);
// CPUS and quiet are in globals.rs
static MAXZOOM: AtomicI32 = AtomicI32::new(32);
static MINZOOM: AtomicI32 = AtomicI32::new(0);
static RENAMES: RwLock<BTreeMap<String, String>> = RwLock::new(BTreeMap::new());
static EXCLUDE_ALL: AtomicBool = AtomicBool::new(false);
static EXCLUDE_ALL_TILE_ATTRIBUTES: AtomicBool = AtomicBool::new(false);
static EXCLUDE_ALL_TILE_GEOMETRIES: AtomicBool = AtomicBool::new(false);
static UNIDECODE_DATA: RwLock<Vec<String>> = RwLock::new(Vec::new());

static WANT_OVERZOOM: AtomicBool = AtomicBool::new(false);
static BUFFER: AtomicI32 = AtomicI32::new(5);

// The C++ defines progress_time() here to always return false. main()
// clears PROGRESS_TIME_ENABLED, which makes tippecanoe_main::progress_time
// do the same.

fn relaxed(v: &AtomicI32) -> i32 {
    v.load(Ordering::Relaxed)
}

/// `std::min` on doubles: `(b < a) ? b : a`.
fn std_min(a: f64, b: f64) -> f64 {
    if b < a { b } else { a }
}

/// `std::max` on doubles: `(a < b) ? b : a`.
fn std_max(a: f64, b: f64) -> f64 {
    if a < b { b } else { a }
}

#[derive(Default)]
struct Stats {
    minzoom: i32,
    maxzoom: i32,
    midlat: f64,
    midlon: f64,
    minlat: f64,
    minlon: f64,
    maxlat: f64,
    maxlon: f64,
    minlat2: f64,
    minlon2: f64,
    maxlat2: f64,
    maxlon2: f64,
}

/// The work of one thread. The C++ also passes the unused join database and
/// the reader queue, which are left out here.
struct Arg<'a> {
    inputs: BTreeMap<Zxy, Vec<Vec<u8>>>,
    outputs: BTreeMap<Zxy, Vec<u8>>,

    layermap: &'a mut BTreeMap<String, LayermapEntry>,

    header: &'a [String],
    mapping: &'a BTreeMap<String, Vec<String>>,
    exclude: &'a BTreeSet<String>,
    include: &'a BTreeSet<String>,
    keep_layers: &'a BTreeSet<String>,
    remove_layers: &'a BTreeSet<String>,
    ifmatched: i32,
    filter: Option<JsonRef<'a>>,

    minlat: f64,
    minlon: f64,
    maxlat: f64,
    maxlon: f64,
    minlon2: f64,
    maxlon2: f64,
}

/// The C++ also takes the layermap, which is `a.layermap`, and the unused join database.
#[allow(clippy::too_many_arguments)]
fn append_tile(
    message: &[u8],
    z: i32,
    x: u32,
    y: u32,
    header: &[String],
    mapping: &BTreeMap<String, Vec<String>>,
    exclude: &BTreeSet<String>,
    include: &BTreeSet<String>,
    keep_layers: &BTreeSet<String>,
    remove_layers: &BTreeSet<String>,
    ifmatched: i32,
    outtile: &mut MvtTile,
    filter: Option<JsonRef<'_>>,
    a: &mut Arg<'_>,
) {
    let mut tile = MvtTile::default();
    let mut was_compressed = false;

    match tile.try_decode(message, &mut was_compressed) {
        Ok(true) => {}
        Ok(false) => {
            eprintln!("Couldn't decompress tile {}/{}/{}", z, x, y);
            std::process::exit(EXIT_MVT);
        }
        Err(_) => {
            eprintln!("PBF decoding error in tile {}/{}/{}", z, x, y);
            std::process::exit(EXIT_MVT);
        }
    }

    let renames = RENAMES.read().unwrap();
    let unidecode_data = UNIDECODE_DATA.read().unwrap();
    let exclude_all = EXCLUDE_ALL.load(Ordering::Relaxed);
    let exclude_all_tile_attributes = EXCLUDE_ALL_TILE_ATTRIBUTES.load(Ordering::Relaxed);
    let exclude_all_tile_geometries = EXCLUDE_ALL_TILE_GEOMETRIES.load(Ordering::Relaxed);
    let pe = relaxed(&PE) != 0;

    for layer in tile.layers.iter_mut() {
        if let Some(found) = renames.get(&layer.name) {
            layer.name = found.clone();
        }
        let layer: &MvtLayer = layer;

        if !keep_layers.is_empty() && !keep_layers.contains(&layer.name) {
            continue;
        }
        if remove_layers.contains(&layer.name) {
            continue;
        }

        let mut ol = 0;
        while ol < outtile.layers.len() {
            if layer.name == outtile.layers[ol].name {
                break;
            }
            ol += 1;
        }

        if ol == outtile.layers.len() {
            outtile.layers.push(MvtLayer::default());

            outtile.layers[ol].name = layer.name.clone();
            outtile.layers[ol].version = layer.version;
            outtile.layers[ol].extent = layer.extent;
        }

        let outlayer = &mut outtile.layers[ol];

        if layer.extent != outlayer.extent && layer.extent > outlayer.extent {
            // this always scales up the existing layer instead of scaling down
            // the layer that is being added, because the assumption is that
            // scaling up is safe while scaling down requires geometry cleaning.

            for feature in outlayer.features.iter_mut() {
                for g in feature.geometry.iter_mut() {
                    g.x = g.x * layer.extent / outlayer.extent;
                    g.y = g.y * layer.extent / outlayer.extent;
                }
            }

            outlayer.extent = layer.extent;
        }

        let mut tilestats_found = a.layermap.contains_key(&layer.name);

        let mut minx = i64::MAX;
        let mut miny = i64::MAX;
        let mut maxx = i64::MIN;
        let mut maxy = i64::MIN;

        let mut minx2 = i64::MAX;
        let mut maxx2 = i64::MIN;
        let mut features_added_to_layer = false;

        for feat in &layer.features {
            let mut exclude_attributes = BTreeSet::new();
            if let Some(filter) = filter
                && !evaluate_feature(
                    feat,
                    layer,
                    filter,
                    &mut exclude_attributes,
                    z,
                    &unidecode_data,
                )
            {
                continue;
            }

            #[derive(Default)]
            struct Match {
                has_id: bool,
                id: u64,
                attributes: BTreeMap<String, (MvtValue, SerialVal)>,
                key_order: Vec<String>,
            }

            let mut matches: Vec<Match> = Vec::new();
            let mut matched = false;

            // look for csv matches and start filling them out

            if !matched {
                let mut m = Match {
                    id: feat.id,
                    has_id: feat.has_id,
                    ..Default::default()
                };
                // populate attributes and key_order as we look for matches,
                // because apparently at some point i thought it was important
                // to insert the joined attributes at the point in the sequence
                // where the join key had been

                for t in (0..feat.tags.len().saturating_sub(1)).step_by(2) {
                    let key = &layer.keys[feat.tags[t] as usize];
                    let val = &layer.values[feat.tags[t + 1] as usize];
                    let sv = mvt_value_to_serial_val(val);

                    if val.type_ == MVT_NULL {
                        continue;
                    }

                    if !exclude_all_tile_attributes
                        && (include.contains(key)
                            || (!exclude_all
                                && !exclude.contains(key)
                                && !exclude_attributes.contains(key)))
                    {
                        m.attributes
                            .entry(key.clone())
                            .or_insert((val.clone(), sv.clone()));
                        m.key_order.push(key.clone());
                    }

                    if !header.is_empty()
                        && *key == header[0]
                        && let Some(fields) = mapping.get(&sv.s)
                    {
                        let fields = fields.clone();
                        matched = true;

                        for i in 1..fields.len() {
                            let joinkey = header[i].clone();
                            let mut joinval = fields[i].clone();
                            let mut attr_type = MVT_STRING;

                            if !joinval.is_empty() {
                                if joinval.as_bytes()[0] == b'"' {
                                    joinval = csv_dequote(joinval);
                                } else if is_number(&joinval) {
                                    attr_type = MVT_DOUBLE;
                                }
                            } else if pe {
                                attr_type = MVT_NULL;
                            }

                            let sjoinkey = joinkey.as_str();

                            if include.contains(&joinkey)
                                || (!exclude_all
                                    && !exclude.contains(&joinkey)
                                    && !exclude_attributes.contains(&joinkey)
                                    && attr_type != MVT_NULL)
                            {
                                let mut outval = MvtValue::default();
                                if attr_type == MVT_STRING {
                                    outval.type_ = MVT_STRING;
                                    outval.set_string_value(&joinval);
                                } else {
                                    outval.type_ = MVT_DOUBLE;
                                    outval.numeric_value.set_double_value(atof(&joinval));
                                }

                                m.attributes.remove(sjoinkey);

                                let outsv = SerialVal {
                                    type_: outval.type_,
                                    s: joinval.clone(),
                                };

                                outval = stringified_to_mvt_value(outval.type_, &joinval);

                                m.attributes
                                    .entry(joinkey.clone())
                                    .or_insert((outval, outsv));
                                m.key_order.push(joinkey);
                            }
                        }
                    }
                }

                if matched {
                    matches.push(m);
                }
            }

            if !matched && ifmatched == 0 {
                // no matches, but they said to keep even unmatched tile features,
                // so make one that is just the original feature

                let mut m = Match {
                    id: feat.id,
                    has_id: feat.has_id,
                    ..Default::default()
                };

                if !exclude_all_tile_attributes {
                    for t in (0..feat.tags.len().saturating_sub(1)).step_by(2) {
                        let key = &layer.keys[feat.tags[t] as usize];
                        let val = &layer.values[feat.tags[t + 1] as usize];
                        let sv = mvt_value_to_serial_val(val);

                        if include.contains(key)
                            || (!exclude_all
                                && !exclude.contains(key)
                                && !exclude_attributes.contains(key))
                        {
                            m.attributes.entry(key.clone()).or_insert((val.clone(), sv));
                            m.key_order.push(key.clone());
                        }
                    }
                }

                matches.push(m);
            }

            for m in matches.iter_mut() {
                if !tilestats_found {
                    let n = a.layermap.len();
                    a.layermap.insert(layer.name.clone(), LayermapEntry::new(n));
                    tilestats_found = true;
                    let tilestats = a.layermap.get_mut(&layer.name).unwrap();
                    tilestats.minzoom = z;
                    tilestats.maxzoom = z;
                }
                let tilestats = a.layermap.get_mut(&layer.name).unwrap();

                let mut outfeature = MvtFeature {
                    id: m.id,
                    has_id: m.has_id,
                    ..Default::default()
                };

                // To keep attributes in their original order instead of alphabetical
                for k in &m.key_order {
                    if let Some(fa) = m.attributes.remove(k) {
                        outlayer.tag(&mut outfeature, k, &fa.0);
                        add_to_tilestats(&mut tilestats.tilestats, k, &fa.1);
                    }
                }

                if exclude_all_tile_geometries {
                    outfeature.type_ = -1;
                } else {
                    outfeature.type_ = feat.type_;
                    outfeature.geometry = feat.geometry.clone();

                    if layer.extent != outlayer.extent {
                        for g in outfeature.geometry.iter_mut() {
                            g.x = g.x * outlayer.extent / layer.extent;
                            g.y = g.y * outlayer.extent / layer.extent;
                        }
                    }

                    for g in &outfeature.geometry {
                        if g.op == MVT_MOVETO || g.op == MVT_LINETO {
                            // pin to the tile extent, since we don't want bounds bigger than the earth
                            let mut gx = std::cmp::min(outlayer.extent, std::cmp::max(0, g.x));
                            let mut gy = std::cmp::min(outlayer.extent, std::cmp::max(0, g.y));

                            // to world scale
                            gx = gx * (1i64 << (32 - z)) / outlayer.extent;
                            gy = gy * (1i64 << (32 - z)) / outlayer.extent;

                            // to world offset
                            gx += (1i64 << (32 - z)) * x as i64;
                            gy += (1i64 << (32 - z)) * y as i64;

                            minx = std::cmp::min(minx, gx);
                            miny = std::cmp::min(miny, gy);
                            maxx = std::cmp::max(maxx, gx);
                            maxy = std::cmp::max(maxy, gy);

                            // if in the western hemisphere, try shifting to east
                            if gx < (1i64 << 31) {
                                gx += 1i64 << 32;
                            }

                            minx2 = std::cmp::min(minx2, gx);
                            maxx2 = std::cmp::max(maxx2, gx);
                        }
                    }
                }

                features_added_to_layer = true;
                outlayer.features.push(outfeature);

                if z < tilestats.minzoom {
                    tilestats.minzoom = z;
                }
                if z > tilestats.maxzoom {
                    tilestats.maxzoom = z;
                }

                if feat.type_ == MVT_POINT {
                    tilestats.points += 1;
                } else if feat.type_ == MVT_LINESTRING {
                    tilestats.lines += 1;
                } else if feat.type_ == MVT_POLYGON {
                    tilestats.polygons += 1;
                }
            }
        }

        if features_added_to_layer {
            let (lon1, lat1) = tile2lonlat(minx, maxy, 32);
            let (lon2, lat2) = tile2lonlat(maxx, miny, 32);

            a.minlat = std_min(a.minlat, std_min(lat1, lat2));
            a.minlon = std_min(a.minlon, std_min(lon1, lon2));
            a.maxlat = std_max(a.maxlat, std_max(lat1, lat2));
            a.maxlon = std_max(a.maxlon, std_max(lon1, lon2));

            let (lon1, _lat1) = tile2lonlat(minx2, maxy, 32);
            let (lon2, _lat2) = tile2lonlat(maxx2, miny, 32);

            a.minlon2 = std_min(a.minlon2, std_min(lon1, lon2));
            a.maxlon2 = std_max(a.maxlon2, std_max(lon1, lon2));
        }
    }
}

/// `tilecmp`, as an ordering for `sort_by`.
fn tilecmp(a: &(u32, u32), b: &(u32, u32)) -> std::cmp::Ordering {
    // must match behavior of tileset_reader::operator<()
    // except backwards, since we are pulling from the end of the list

    if b.0 < a.0 {
        return std::cmp::Ordering::Less;
    }
    if b.0 == a.0 {
        // Y sorts backwards, in TMS order
        if b.1 > a.1 {
            return std::cmp::Ordering::Less;
        }
        if b.1 == a.1 {
            return std::cmp::Ordering::Equal;
        }
    }

    std::cmp::Ordering::Greater
}

// The `tileset_reader` is an iterator through the tiles of a tileset,
// in z/x/tms_y order.
//
// The basic idea is that it is used like this:
//
//  void blah(const char *fname) {
//      tileset_reader r(fname);
//
//      for (; !r.all_done(); r.advance()) {
//          std::pair<zxy, std::string> tile = r.current();
//          whatever(tile);
//      }
//
//      r.close();
//  }
//
// The complication is that you can actually keep calling current()
// and advance() after the tileset_reader claims to be done, in which case
// it will produce overzoomed tiles generated from the tiles in
// the maxzoom tileset. The parent tiles for those overzoomed tiles
// are retrieved internally using get_tile() rather than through the
// main iteration query.

struct TilesetReader {
    // z/x/y and data of the current tile
    zoom: i64,
    x: i64,
    y: i64,
    data: Vec<u8>,
    current_tile_is_overzoomed: bool,

    // "done" means we have read all of the real tiles from the source.
    // The iterator will continue to produce overzoomed tiles after it is "done."
    done: bool,

    // for overzooming
    maxzoom_so_far: i32,
    tiles_at_maxzoom_so_far: Vec<(u32, u32)>,
    overzoomed_tiles: Vec<(u32, u32)>,      // tiles at `zoom`
    next_overzoomed_tiles: Vec<(u32, u32)>, // tiles at `zoom + 1`
    overzoom_consumed_at_this_zoom: bool,

    // parent tile cache
    cache: TileCache,

    // for iterating mbtiles
    // (the statement is declared before the database so that it is finalized first)
    stmt: Option<RawStmt>,
    db: Option<Connection>,
    next: Option<Box<TilesetReader>>,

    // for iterating dirtiles
    dirtiles: VecDeque<Zxy>,
    dirbase: String,
    name: String,

    // for iterating pmtiles
    pmtiles_map: Option<Mmap>,
    pmtiles_entries: Vec<EntryZxy>,
}

impl TilesetReader {
    fn new(fname: &str) -> TilesetReader {
        let mut r = TilesetReader {
            zoom: 0,
            x: 0,
            y: 0,
            data: Vec::new(),
            current_tile_is_overzoomed: false,
            done: false,
            maxzoom_so_far: -1,
            tiles_at_maxzoom_so_far: Vec::new(),
            overzoomed_tiles: Vec::new(),
            next_overzoomed_tiles: Vec::new(),
            overzoom_consumed_at_this_zoom: false,
            cache: TileCache::default(),
            stmt: None,
            db: None,
            next: None,
            dirtiles: VecDeque::new(),
            dirbase: String::new(),
            name: fname.to_string(),
            pmtiles_map: None,
            pmtiles_entries: Vec::new(),
        };

        let minzoom = relaxed(&MINZOOM);
        let maxzoom = relaxed(&MAXZOOM);

        if std::fs::metadata(fname).is_ok_and(|st| (st.mode() & libc::S_IFDIR as u32) != 0) {
            r.db = None;
            r.stmt = None;
            r.next = None;

            r.dirtiles = enumerate_dirtiles(fname, minzoom, maxzoom).into();
            r.dirbase = fname.to_string();
        } else if pmtiles_has_suffix(fname) {
            let map = match File::open(fname) {
                Ok(pmtiles_fd) => {
                    let len = pmtiles_fd.metadata().map(|st| st.size()).unwrap_or(0);
                    mmap_file(&pmtiles_fd, len)
                }
                // mmap() of the -1 that open() returned
                Err(_) => Err(libc::EBADF),
            };
            let map = match map {
                Ok(map) => map,
                Err(errno) => {
                    eprintln!("mmap in decode: {}", strerror(errno));
                    std::process::exit(EXIT_MEMORY);
                }
            };

            r.pmtiles_entries = pmtiles_entries_tms(&map, minzoom, maxzoom);
            r.pmtiles_entries.reverse();
            r.pmtiles_map = Some(map);
        } else {
            let db = match sqlite3_open(fname) {
                Ok(db) => db,
                Err(msg) => {
                    eprintln!("{}: {}", fname, msg);
                    std::process::exit(EXIT_SQLITE);
                }
            };

            if let Err(e) = db.execute_batch("PRAGMA integrity_check;") {
                eprintln!("{}: integrity_check: {}", fname, e);
                std::process::exit(EXIT_SQLITE);
            }

            let sql = c"SELECT zoom_level, tile_column, tile_row, tile_data from tiles order by zoom_level, tile_column, tile_row;";

            let query = match RawStmt::prepare(&db, sql) {
                Ok(query) => query,
                Err(msg) => {
                    eprintln!("{}: select failed: {}", fname, msg);
                    std::process::exit(EXIT_SQLITE);
                }
            };

            r.stmt = Some(query);
            r.db = Some(db);
            r.next = None;
        }

        r
    }

    // Checks the done status not only of this tileset_reader but also
    // the others chained to it in the queue.
    //
    // Also claims not to be done if at least one overzoomed tile
    // has been consumed at this zoom level, in which case they should
    // all allowed to be consumed before stopping.
    fn all_done(&self) -> bool {
        if !self.done {
            return false;
        }
        if self.overzoom_consumed_at_this_zoom {
            return false;
        }

        let mut r = self.next.as_deref();
        while let Some(rr) = r {
            if !rr.done {
                return false;
            }
            if rr.overzoom_consumed_at_this_zoom {
                return false;
            }
            r = rr.next.as_deref();
        }
        true
    }

    fn current(&mut self) -> (Zxy, Vec<u8>) {
        if self.current_tile_is_overzoomed {
            self.overzoom_consumed_at_this_zoom = true;
        }

        (
            Zxy::new(self.zoom as i32, self.x as i32, self.y as i32),
            self.data.clone(),
        )
    }

    fn advance(&mut self) {
        if self.done {
            if !WANT_OVERZOOM.load(Ordering::Relaxed) {
                eprintln!("overzoom advance called without -O");
                std::process::exit(EXIT_IMPOSSIBLE);
            }

            if self.overzoomed_tiles.is_empty() {
                self.next_overzoom();
                self.overzoom_consumed_at_this_zoom = false;
            }

            let xy = match self.overzoomed_tiles.pop() {
                Some(xy) => xy,
                None => {
                    // we have nothing to overzoom; give up
                    self.current_tile_is_overzoomed = false;
                    self.zoom = 32;
                    return;
                }
            };

            self.x = xy.0 as i64;
            self.y = xy.1 as i64;
            self.data =
                self.retrieve_overzoom(Zxy::new(self.zoom as i32, self.x as i32, self.y as i32));
            self.current_tile_is_overzoomed = true;

            return;
        }

        self.current_tile_is_overzoomed = false;

        if let Some(stmt) = &mut self.stmt {
            if stmt.step() == ffi::SQLITE_ROW {
                self.zoom = stmt.column_int(0) as i64;
                self.x = stmt.column_int(1) as i64;
                let tms_y = stmt.column_int(2);
                self.y = 1i64
                    .wrapping_shl(self.zoom as u32)
                    .wrapping_sub(1)
                    .wrapping_sub(tms_y as i64);
                self.data = stmt.column_blob(3).to_vec();
            } else {
                self.done = true;
            }
        } else if let Some(pmtiles_map) = &self.pmtiles_map {
            match self.pmtiles_entries.pop() {
                None => self.done = true,
                Some(back) => {
                    self.zoom = back.z as i64;
                    self.x = back.x as i64;
                    self.y = back.y as i64;
                    self.data = pmtiles_map
                        [back.offset as usize..back.offset as usize + back.length as usize]
                        .to_vec();
                }
            }
        } else {
            match self.dirtiles.pop_front() {
                None => self.done = true,
                Some(front) => {
                    self.zoom = front.z;
                    self.x = front.x;
                    self.y = front.y;
                    self.data = dir_read_tile(&self.dirbase, &front);
                }
            }
        }

        if self.done {
            if WANT_OVERZOOM.load(Ordering::Relaxed) {
                self.next_overzoom();
                self.advance();
            } else {
                self.zoom = 32;
            }
        } else {
            if self.zoom > self.maxzoom_so_far as i64 {
                self.maxzoom_so_far = self.zoom as i32;
                self.tiles_at_maxzoom_so_far.clear();
            }

            if WANT_OVERZOOM.load(Ordering::Relaxed) {
                self.tiles_at_maxzoom_so_far
                    .push((self.x as u32, self.y as u32));
            }
        }
    }

    fn close(&mut self) {
        if let Some(pmtiles_map) = &self.pmtiles_map {
            self.db = Some(pmtilesmeta2tmp(&self.name, pmtiles_map));
            // json, strategies
        } else if self.db.is_none() {
            self.db = Some(dirmeta2tmp(&self.dirbase));
        } else {
            // sqlite3_finalize()
            self.stmt = None;
        }
    }

    fn next_overzoom(&mut self) {
        self.zoom += 1;
        self.overzoomed_tiles.clear();

        // +1 because maxzoom_so_far is initially -1, an invalid shift
        let scale: i64 = (1i64 << (self.zoom + 1)) / (1i64 << (self.maxzoom_so_far + 1));

        // If this is the first overzoomed level, we don't know yet
        // which tiles will be useful, so spell out all 4 child tiles
        // from each parent tile.
        //
        // If it is further overzoomed than that, we have a list of
        // which child tiles will have features in them, so use that.

        if self.zoom == self.maxzoom_so_far as i64 + 1 {
            for xy in &self.tiles_at_maxzoom_so_far {
                for xx in 0..scale {
                    for yy in 0..scale {
                        self.overzoomed_tiles.push((
                            (xy.0 as i64 * scale + xx) as u32,
                            (xy.1 as i64 * scale + yy) as u32,
                        ));
                    }
                }
            }
        } else {
            self.overzoomed_tiles = std::mem::take(&mut self.next_overzoomed_tiles);
            self.next_overzoomed_tiles.clear();
        }

        self.overzoomed_tiles.sort_by(tilecmp);
        self.overzoom_consumed_at_this_zoom = false;
    }

    fn get_tile(&self, tile: Zxy) -> MvtTile {
        let mut source: Vec<u8> = Vec::new();

        if let Some(db) = &self.db {
            let sql = c"SELECT tile_data from tiles where zoom_level = ? and tile_column = ? and tile_row = ?;";
            let mut query = match RawStmt::prepare(db, sql) {
                Ok(query) => query,
                Err(msg) => {
                    eprintln!("{}: select failed: {}", self.name, msg);
                    std::process::exit(EXIT_SQLITE);
                }
            };

            query.bind_int(1, tile.z as i32);
            query.bind_int(2, tile.x as i32);
            query.bind_int(
                3,
                1i64.wrapping_shl(tile.z as u32)
                    .wrapping_sub(1)
                    .wrapping_sub(tile.y) as i32,
            );

            if query.step() == ffi::SQLITE_ROW {
                source = query.column_blob(0).to_vec();
            }
        } else if let Some(pmtiles_map) = &self.pmtiles_map {
            let (tile_offset, tile_length) =
                pmtiles_get_tile(pmtiles_map, tile.z as i32, tile.x as i32, tile.y as i32);
            if tile_length > 0 {
                source = pmtiles_map
                    [tile_offset as usize..tile_offset as usize + tile_length as usize]
                    .to_vec();
            }
        } else {
            source = dir_read_tile(&self.dirbase, &tile);
        }

        let mut content = MvtTile::default();
        if source.is_empty() {
            return content;
        }

        let mut was_compressed = false;
        match content.try_decode(&source, &mut was_compressed) {
            Ok(true) => {}
            Ok(false) => {
                eprintln!("Couldn't parse tile {}/{}/{}", tile.z, tile.x, tile.y);
                std::process::exit(EXIT_MVT);
            }
            Err(_) => {
                eprintln!(
                    "PBF decoding error in tile {}/{}/{}",
                    tile.z, tile.x, tile.y
                );
                std::process::exit(EXIT_PROTOBUF);
            }
        }

        content
    }

    // Sort in z/x/tms_y order, because that is the order of the
    // straightforward query of the mbtiles tiles table.
    /// `operator<`
    fn lt(&self, r: &TilesetReader) -> bool {
        // must match behavior of tilecmp

        if self.zoom < r.zoom {
            return true;
        }
        if self.zoom > r.zoom {
            return false;
        }

        if self.x < r.x {
            return true;
        }
        if self.x > r.x {
            return false;
        }

        let sorty = 1i64
            .wrapping_shl(self.zoom as u32)
            .wrapping_sub(1)
            .wrapping_sub(self.y) as i32;
        let r_sorty = 1i64
            .wrapping_shl(r.zoom as u32)
            .wrapping_sub(1)
            .wrapping_sub(r.y) as i32;

        if sorty < r_sorty {
            return true;
        }
        if sorty > r_sorty {
            return false;
        }

        if self.data < r.data {
            return true;
        }

        false
    }

    fn retrieve_overzoom(&mut self, tile: Zxy) -> Vec<u8> {
        // lock around sqlite3 access
        static RETRIEVE_LOCK: Mutex<()> = Mutex::new(());

        let mut parent_tile = tile.clone();
        while parent_tile.z > self.maxzoom_so_far as i64 {
            parent_tile.z -= 1;
            parent_tile.x /= 2;
            parent_tile.y /= 2;
        }

        let guard = RETRIEVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let mut cache = std::mem::take(&mut self.cache);
        let mut getter = |tileno: Zxy| self.get_tile(tileno);

        let source = cache.get(parent_tile.clone(), &mut getter);
        self.cache = cache;

        drop(guard);

        if !source.layers.is_empty() {
            let tv = vec![SourceTile {
                tile: source,
                z: parent_tile.z as i32,
                x: parent_tile.x as i32,
                y: parent_tile.y as i32,
            }];

            let unidecode_data = UNIDECODE_DATA.read().unwrap();
            return overzoom_source_tiles(
                &tv,
                tile.z as i32,
                tile.x as i32,
                tile.y as i32,
                -1,
                relaxed(&BUFFER),
                &BTreeSet::new(),
                &BTreeSet::new(),
                &[],
                false,
                Some(&mut self.next_overzoomed_tiles),
                false,
                None,
                false,
                &HashMap::new(),
                &unidecode_data,
                0.0,
                0.0,
                &[],
                "",
                "",
                usize::MAX,
                &[],
                false,
            );
        }

        Vec::new()
    }
}

fn begin_reading(fname: &str) -> Box<TilesetReader> {
    let mut r = Box::new(TilesetReader::new(fname));

    // The reason this prefetches is so the tileset_reader queue can be
    // priority-ordered, so the one with the next relevant tile
    // is first in line.
    r.advance();
    r
}

/// Links `r` into the queue `readers` before the first reader that it sorts before.
fn insert_reader(readers: &mut Option<Box<TilesetReader>>, mut r: Box<TilesetReader>) {
    let mut rr = readers;
    while rr.as_ref().is_some_and(|here| !r.lt(here)) {
        rr = &mut rr.as_mut().unwrap().next;
    }

    r.next = rr.take();
    *rr = Some(r);
}

fn join_worker(a: &mut Arg<'_>) {
    let inputs = std::mem::take(&mut a.inputs);
    let (header, mapping, exclude, include) = (a.header, a.mapping, a.exclude, a.include);
    let (keep_layers, remove_layers, ifmatched, filter) =
        (a.keep_layers, a.remove_layers, a.ifmatched, a.filter);

    for (key, messages) in inputs {
        let mut tile = MvtTile::default();

        for message in &messages {
            append_tile(
                message,
                key.z as i32,
                key.x as u32,
                key.y as u32,
                header,
                mapping,
                exclude,
                include,
                keep_layers,
                remove_layers,
                ifmatched,
                &mut tile,
                filter,
                a,
            );
        }

        drop(messages);

        let mut anything = false;
        let mut outtile = MvtTile::default();
        for layer in tile.layers {
            if !layer.features.is_empty() {
                outtile.layers.push(layer);
                anything = true;
            }
        }

        if anything {
            let pbf = outtile.encode();
            let mut compressed = Vec::new();

            if relaxed(&PC) == 0 {
                compress(&pbf, &mut compressed, true);
            } else {
                compressed = pbf;
            }

            if relaxed(&PK) == 0 && compressed.len() > 500000 {
                eprintln!(
                    "Tile {}/{}/{} size is {}, >500000. Skipping this tile.",
                    key.z,
                    key.x,
                    key.y,
                    compressed.len() as i64
                );
            } else {
                a.outputs.entry(key).or_insert(compressed);
            }
        }
    }
}

/// The C++ also takes the unused join database and the reader queue.
#[allow(clippy::too_many_arguments)]
fn dispatch_tasks(
    tasks: &mut BTreeMap<Zxy, Vec<Vec<u8>>>,
    layermaps: &mut [BTreeMap<String, LayermapEntry>],
    outdb: Option<&Connection>,
    outdir: Option<&str>,
    header: &[String],
    mapping: &BTreeMap<String, Vec<String>>,
    exclude: &BTreeSet<String>,
    include: &BTreeSet<String>,
    ifmatched: i32,
    keep_layers: &BTreeSet<String>,
    remove_layers: &BTreeSet<String>,
    filter: Option<JsonRef<'_>>,
    minlat: &mut f64,
    minlon: &mut f64,
    maxlat: &mut f64,
    maxlon: &mut f64,
    minlon2: &mut f64,
    maxlon2: &mut f64,
) {
    let cpus = CPUS.load(Ordering::Relaxed);
    let mut args: Vec<Arg<'_>> = Vec::new();

    for layermap in layermaps.iter_mut().take(cpus) {
        args.push(Arg {
            inputs: BTreeMap::new(),
            outputs: BTreeMap::new(),
            layermap,
            header,
            mapping,
            exclude,
            include,
            keep_layers,
            remove_layers,
            ifmatched,
            filter,
            minlat: *minlat,
            minlon: *minlon,
            maxlat: *maxlat,
            maxlon: *maxlon,
            minlon2: *minlon2,
            maxlon2: *maxlon2,
        });
    }

    let mut count = 0;
    // This isn't careful about distributing tasks evenly across CPUs,
    // but, from testing, it actually takes a little longer to do
    // the proper allocation than is saved by perfectly balanced threads.
    let mut first = true;
    for (key, value) in std::mem::take(tasks) {
        let (z, x, y) = (key.z, key.x, key.y);
        args[count].inputs.insert(key, value);
        count = (count + 1) % cpus;

        if first {
            if relaxed(&QUIET) == 0 {
                eprint!("{}/{}/{}  \r", z, x, y);
            }
            first = false;
        }
    }

    std::thread::scope(|s| {
        let mut pthreads = Vec::new();
        for mut a in args {
            pthreads.push(thread_create(s, move || {
                join_worker(&mut a);
                a
            }));
        }

        for pthread in pthreads {
            let a = match pthread.join() {
                Ok(a) => a,
                Err(e) => std::panic::resume_unwind(e),
            };

            *minlat = std_min(*minlat, a.minlat);
            *minlon = std_min(*minlon, a.minlon);
            *maxlat = std_max(*maxlat, a.maxlat);
            *maxlon = std_max(*maxlon, a.maxlon);
            *minlon2 = std_min(*minlon2, a.minlon2);
            *maxlon2 = std_max(*maxlon2, a.maxlon2);

            for (key, data) in &a.outputs {
                if let Some(outdb) = outdb {
                    mbtiles_write_tile(outdb, key.z as i32, key.x as i32, key.y as i32, data);
                } else if let Some(outdir) = outdir {
                    dir_write_tile(outdir, key.z as i32, key.x as i32, key.y as i32, data);
                }
            }
        }
    });
}

fn handle_strategies(s: Option<&[u8]>, st: &mut Vec<Strategy>) {
    let s = s.unwrap_or(b"");
    let ss = String::from_utf8_lossy(s);
    let mut jp = JsonPull::begin_string(s);
    let o = jp.read_tree();

    let Some(o) = &o else {
        return;
    };
    let o = o.root();

    if o.type_() == JsonType::Array {
        for (i, h) in o.array().iter().enumerate() {
            if h.type_() == JsonType::Hash {
                for (j, (key, value)) in h.entries().iter().enumerate() {
                    if key.type_() != JsonType::String {
                        eprintln!("Key {} of {} is not a string: {}", j, i, ss);
                    } else if value.type_() != JsonType::Number {
                        eprintln!("Value {} of {} is not a number: {}", j, i, ss);
                    } else {
                        if i >= st.len() {
                            st.resize(i + 1, Strategy::default());
                        }

                        let n = value.number();
                        let add = |v: &mut usize| *v = (*v as f64 + n) as usize;
                        match key.string() {
                            "dropped_by_rate" => add(&mut st[i].dropped_by_rate),
                            "dropped_by_gamma" => add(&mut st[i].dropped_by_gamma),
                            "dropped_as_needed" => add(&mut st[i].dropped_as_needed),
                            "coalesced_as_needed" => add(&mut st[i].coalesced_as_needed),
                            "truncated_zooms" => add(&mut st[i].truncated_zooms),
                            "detail_reduced" => add(&mut st[i].detail_reduced),
                            "tiny_polygons" => add(&mut st[i].tiny_polygons),
                            "tile_size_desired" => add(&mut st[i].tile_size),
                            "feature_count_desired" => add(&mut st[i].feature_count),
                            _ => {}
                        }
                    }
                }
            } else {
                eprintln!("Element {} is not a hash: {}", i, ss);
            }
        }
    }
}

fn handle_vector_layers(
    vector_layers: Option<JsonRef<'_>>,
    layermap: &mut BTreeMap<String, LayermapEntry>,
    attribute_descriptions: &mut BTreeMap<String, String>,
) {
    let Some(vector_layers) = vector_layers else {
        return;
    };
    if vector_layers.type_() != JsonType::Array {
        return;
    }

    for layer in vector_layers.array() {
        if layer.type_() == JsonType::Hash {
            let id = layer.hash_get("id");
            let desc = layer.hash_get("description");

            if let (Some(id), Some(desc)) = (id, desc)
                && id.type_() == JsonType::String
                && desc.type_() == JsonType::String
            {
                let sid = id.string();
                let sdesc = desc.string();

                if !sdesc.is_empty()
                    && let Some(f) = layermap.get_mut(sid)
                {
                    f.description = sdesc.to_string();
                }
            }

            let fields = layer.hash_get("fields");
            if let Some(fields) = fields
                && fields.type_() == JsonType::Hash
            {
                for (key, value) in fields.entries() {
                    if key.type_() == JsonType::String && value.type_() == JsonType::String {
                        let desc2 = value.string();

                        if desc2 != "Number"
                            && desc2 != "String"
                            && desc2 != "Boolean"
                            && desc2 != "Mixed"
                        {
                            attribute_descriptions
                                .entry(key.string().to_string())
                                .or_insert_with(|| desc2.to_string());
                        }
                    }
                }
            }
        }
    }
}

/// `sscanf(s, "%lf,%lf", a, b)`, which assigns as many values as it could read.
fn scan_lf_comma_lf(s: &str, a: &mut f64, b: &mut f64) {
    let (v, n) = strtod(s);
    if n == 0 {
        return;
    }
    *a = v;

    let Some(rest) = s[n..].strip_prefix(',') else {
        return;
    };
    let (v, n) = strtod(rest);
    if n == 0 {
        return;
    }
    *b = v;
}

/// The C++ also takes the unused join database.
#[allow(clippy::too_many_arguments)]
fn decode(
    mut readers: Option<Box<TilesetReader>>,
    layermap: &mut BTreeMap<String, LayermapEntry>,
    outdb: Option<&Connection>,
    outdir: Option<&str>,
    st: &mut Stats,
    header: &[String],
    mapping: &BTreeMap<String, Vec<String>>,
    exclude: &BTreeSet<String>,
    include: &BTreeSet<String>,
    ifmatched: i32,
    attribution: &mut String,
    description: &mut String,
    keep_layers: &BTreeSet<String>,
    remove_layers: &BTreeSet<String>,
    name: &mut String,
    filter: Option<JsonRef<'_>>,
    attribute_descriptions: &mut BTreeMap<String, String>,
    generator_options: &mut String,
    strategies: &mut Vec<Strategy>,
) {
    let cpus = CPUS.load(Ordering::Relaxed);
    let minzoom = relaxed(&MINZOOM);
    let maxzoom = relaxed(&MAXZOOM);
    let want_overzoom = WANT_OVERZOOM.load(Ordering::Relaxed);

    let mut layermaps: Vec<BTreeMap<String, LayermapEntry>> = Vec::new();
    for _ in 0..cpus {
        layermaps.push(BTreeMap::new());
    }

    let mut tasks: BTreeMap<Zxy, Vec<Vec<u8>>> = BTreeMap::new();
    let mut minlat = i32::MAX as f64;
    let mut minlon = i32::MAX as f64;
    let mut maxlat = i32::MIN as f64;
    let mut maxlon = i32::MIN as f64;
    let mut minlon2 = i32::MAX as f64;
    let mut maxlon2 = i32::MIN as f64;

    while readers.as_ref().is_some_and(|r| !r.all_done()) {
        let mut r = readers.take().unwrap();
        let current = r.current();

        if current.0.z >= minzoom as i64 && current.0.z <= maxzoom as i64 {
            let tile = current.0.clone();
            tasks.entry(tile).or_default().push(current.1);
        }

        // Advance the tileset_reader that we just added as a task.
        // The reason this prefetches is so the tileset_reader queue can be
        // priority-ordered, so the one with the next relevant tile
        // is first in line.
        r.advance();

        // pull the tileset_reader off the front of the queue for reordering

        readers = r.next.take();

        // Is the next tileset_reader on the tileset_reader queue looking at a different tile?
        // Then this tile is done and we can safely run the output queue.

        if readers
            .as_ref()
            .is_none_or(|n| n.zoom != current.0.z || n.x != current.0.x || n.y != current.0.y)
            && tasks.len() > 100 * cpus
        {
            dispatch_tasks(
                &mut tasks,
                &mut layermaps,
                outdb,
                outdir,
                header,
                mapping,
                exclude,
                include,
                ifmatched,
                keep_layers,
                remove_layers,
                filter,
                &mut minlat,
                &mut minlon,
                &mut maxlat,
                &mut maxlon,
                &mut minlon2,
                &mut maxlon2,
            );
            tasks.clear();
        }

        // put the tileset_reader back onto the queue,
        // in whatever sequence its next tile calls for

        insert_reader(&mut readers, r);
    }

    dispatch_tasks(
        &mut tasks,
        &mut layermaps,
        outdb,
        outdir,
        header,
        mapping,
        exclude,
        include,
        ifmatched,
        keep_layers,
        remove_layers,
        filter,
        &mut minlat,
        &mut minlon,
        &mut maxlat,
        &mut maxlon,
        &mut minlon2,
        &mut maxlon2,
    );
    *layermap = merge_layermaps(&layermaps);

    st.minlon = std_min(minlon, st.minlon);
    st.maxlon = std_max(maxlon, st.maxlon);
    st.minlat = std_min(minlat, st.minlat);
    st.maxlat = std_max(maxlat, st.maxlat);

    st.minlon2 = std_min(minlon2, st.minlon2);
    st.maxlon2 = std_max(maxlon2, st.maxlon2);
    st.minlat2 = std_min(minlat, st.minlat2);
    st.maxlat2 = std_max(maxlat, st.maxlat2);

    let mut next;
    let mut cur = readers;
    while let Some(mut r) = cur {
        next = r.next.take();
        r.close();

        {
            let db = r.db.as_ref().unwrap();

            if let Ok(mut stmt) =
                RawStmt::prepare(db, c"SELECT value from metadata where name = 'minzoom'")
                && stmt.step() == ffi::SQLITE_ROW
            {
                let minz = std::cmp::max(stmt.column_int(0), minzoom);
                st.minzoom = std::cmp::min(st.minzoom, minz);
            }
            if let Ok(mut stmt) =
                RawStmt::prepare(db, c"SELECT value from metadata where name = 'maxzoom'")
                && stmt.step() == ffi::SQLITE_ROW
            {
                let maxz = std::cmp::min(stmt.column_int(0), maxzoom);

                if !want_overzoom && st.maxzoom >= 0 && maxz != st.maxzoom {
                    eprintln!(
                        "Warning: mismatched maxzooms: {} in {} vs previous {}",
                        maxz, r.name, st.maxzoom
                    );
                }

                st.maxzoom = std::cmp::max(st.maxzoom, maxz);
            }
            if let Ok(mut stmt) =
                RawStmt::prepare(db, c"SELECT value from metadata where name = 'center'")
                && stmt.step() == ffi::SQLITE_ROW
                && let Some(s) = stmt.column_text(0)
            {
                scan_lf_comma_lf(&String::from_utf8_lossy(s), &mut st.midlon, &mut st.midlat);
            }
            if let Ok(mut stmt) =
                RawStmt::prepare(db, c"SELECT value from metadata where name = 'attribution'")
                && stmt.step() == ffi::SQLITE_ROW
                && let Some(s) = stmt.column_text(0)
            {
                *attribution = String::from_utf8_lossy(s).into_owned();
            }
            if let Ok(mut stmt) =
                RawStmt::prepare(db, c"SELECT value from metadata where name = 'description'")
                && stmt.step() == ffi::SQLITE_ROW
                && let Some(s) = stmt.column_text(0)
            {
                *description = String::from_utf8_lossy(s).into_owned();
            }
            if let Ok(mut stmt) =
                RawStmt::prepare(db, c"SELECT value from metadata where name = 'name'")
                && stmt.step() == ffi::SQLITE_ROW
                && let Some(s) = stmt.column_text(0)
            {
                let s = String::from_utf8_lossy(s);
                if name.is_empty() {
                    *name = s.into_owned();
                } else {
                    let proposed = format!("{} + {}", name, s);
                    if proposed.len() < 255 {
                        *name = proposed;
                    }
                }
            }
            if let Ok(mut stmt) =
                RawStmt::prepare(db, c"SELECT value from metadata where name = 'json'")
                && stmt.step() == ffi::SQLITE_ROW
                && let Some(s) = stmt.column_text(0)
            {
                let mut jp = JsonPull::begin_string(s);
                let o = jp.read_tree();

                if let Some(o) = &o
                    && o.root().type_() == JsonType::Hash
                {
                    let vector_layers = o.root().hash_get("vector_layers");

                    handle_vector_layers(vector_layers, layermap, attribute_descriptions);
                }
            }
            if let Ok(mut stmt) = RawStmt::prepare(
                db,
                c"SELECT value from metadata where name = 'generator_options'",
            ) && stmt.step() == ffi::SQLITE_ROW
                && let Some(s) = stmt.column_text(0)
            {
                let s = String::from_utf8_lossy(s);
                if !generator_options.is_empty() {
                    generator_options.push_str("; ");
                    generator_options.push_str(&s);
                } else {
                    *generator_options = s.into_owned();
                }
            }
            if let Ok(mut stmt) =
                RawStmt::prepare(db, c"SELECT value from metadata where name = 'strategies'")
                && stmt.step() == ffi::SQLITE_ROW
            {
                let s = stmt.column_text(0);
                handle_strategies(s, strategies);
            }
        }

        // Closes either real r->db or temp mirror of metadata.json
        if let Some(db) = r.db.take()
            && let Err((_, e)) = db.close()
        {
            eprintln!("Could not close database: {}", e);
            std::process::exit(EXIT_CLOSE);
        }

        drop(r);
        cur = next;
    }
}

const fn opt(
    name: &'static str,
    has_arg: i32,
    flag: Option<&'static AtomicI32>,
    val: u8,
) -> LongOption {
    LongOption {
        name,
        has_arg,
        flag,
        val: val as i32,
    }
}

static LONG_OPTIONS: [LongOption; 44] = [
    opt("Output tileset", 0, None, 0),
    opt("output", REQUIRED_ARGUMENT, None, b'o'),
    opt("output-to-directory", REQUIRED_ARGUMENT, None, b'e'),
    opt("force", NO_ARGUMENT, None, b'f'),
    //
    opt("Tileset description and attribution", 0, None, 0),
    opt("name", REQUIRED_ARGUMENT, None, b'n'),
    opt("attribution", REQUIRED_ARGUMENT, None, b'A'),
    opt("description", REQUIRED_ARGUMENT, None, b'N'),
    //
    opt("Input tilesets", 0, None, 0),
    opt("read-from", REQUIRED_ARGUMENT, None, b'r'),
    //
    opt("Zoom levels", 0, None, 0),
    opt("maximum-zoom", REQUIRED_ARGUMENT, None, b'z'),
    opt("minimum-zoom", REQUIRED_ARGUMENT, None, b'Z'),
    opt("overzoom", NO_ARGUMENT, None, b'O'),
    opt("buffer", REQUIRED_ARGUMENT, None, b'b'),
    //
    opt("Layer names", 0, None, 0),
    opt("layer", REQUIRED_ARGUMENT, None, b'l'),
    opt("exclude-layer", REQUIRED_ARGUMENT, None, b'L'),
    opt("rename-layer", REQUIRED_ARGUMENT, None, b'R'),
    //
    opt("Joining with a CSV file", 0, None, 0),
    opt("csv", REQUIRED_ARGUMENT, None, b'c'),
    opt("if-matched", NO_ARGUMENT, None, b'i'),
    opt("empty-csv-columns-are-null", NO_ARGUMENT, Some(&PE), 1),
    //
    opt("Filtering feature attributes", 0, None, 0),
    opt("exclude", REQUIRED_ARGUMENT, None, b'x'),
    opt("include", REQUIRED_ARGUMENT, None, b'y'),
    opt("exclude-all", NO_ARGUMENT, None, b'X'),
    opt("exclude-all-tile-attributes", NO_ARGUMENT, None, b'~'),
    opt("exclude-all-tile-geometries", NO_ARGUMENT, None, b'~'),
    //
    opt("Filtering features by attributes", 0, None, 0),
    opt("feature-filter-file", REQUIRED_ARGUMENT, None, b'J'),
    opt("feature-filter", REQUIRED_ARGUMENT, None, b'j'),
    //
    opt("Setting or disabling tile size limits", 0, None, 0),
    opt("no-tile-size-limit", NO_ARGUMENT, Some(&PK), 1),
    opt("no-tile-compression", NO_ARGUMENT, Some(&PC), 1),
    opt("no-tile-stats", NO_ARGUMENT, Some(&PG), 1),
    opt("tile-stats-attributes-limit", REQUIRED_ARGUMENT, None, b'~'),
    opt(
        "tile-stats-sample-values-limit",
        REQUIRED_ARGUMENT,
        None,
        b'~',
    ),
    opt("tile-stats-values-limit", REQUIRED_ARGUMENT, None, b'~'),
    //
    opt("Progress indicator", 0, None, 0),
    opt("quiet", NO_ARGUMENT, None, b'q'),
    //
    opt("", 0, None, 0),
    opt("prevent", REQUIRED_ARGUMENT, None, b'p'),
    opt("unidecode-data", REQUIRED_ARGUMENT, None, b'~'),
];

fn usage(argv: &[String]) -> ! {
    let forms = [
        "[options] source.mbtiles ...",
        "[options] --read-from=inputfile.txt",
    ];
    let required = [
        UsageRequiredOption {
            name: "output",
            placeholder: "new.mbtiles",
            alternation: 1,
        },
        UsageRequiredOption {
            name: "output-to-directory",
            placeholder: "directory",
            alternation: 1,
        },
    ];

    print_usage(
        &mut std::io::stderr(),
        &argv[0],
        &forms,
        &LONG_OPTIONS,
        &required,
    );
    std::process::exit(EXIT_ARGS);
}

/// The lines that `getline()` returns one by one.
fn getline_lines(content: &[u8]) -> Vec<&[u8]> {
    let mut lines: Vec<&[u8]> = content.split(|&c| c == b'\n').collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

pub fn main() {
    PROGRESS_TIME_ENABLED.store(false, Ordering::Relaxed);
    restore_sigpipe();

    let mut argv: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let argc = argv.len();
    let mut out_mbtiles: Option<String> = None;
    let mut out_dir: Option<String> = None;
    let mut outdb: Option<Connection> = None;
    let mut csv: Option<String> = None;
    let mut force = 0;
    let mut ifmatched = 0;
    let mut filearg = 0;
    let mut filter: Option<JsonTree> = None;

    let mut readers: Option<Box<TilesetReader>> = None;

    let mut cpus = get_num_avail_cpus() as usize;

    if let Some(max_threads) = std::env::var_os("TIPPECANOE_MAX_THREADS") {
        cpus = atoi(&max_threads.to_string_lossy()) as usize;
    }
    if cpus < 1 {
        cpus = 1;
    }
    CPUS.store(cpus, Ordering::Relaxed);

    // PORT: the C++ calls sqlite3_config(SQLITE_CONFIG_SERIALIZED) here. Each
    // rusqlite connection is only used by one thread at a time, so it isn't needed.

    let mut header: Vec<String> = Vec::new();
    let mut mapping: BTreeMap<String, Vec<String>> = BTreeMap::new();

    let mut exclude: BTreeSet<String> = BTreeSet::new();
    let mut include: BTreeSet<String> = BTreeSet::new();
    let mut keep_layers: BTreeSet<String> = BTreeSet::new();
    let mut remove_layers: BTreeSet<String> = BTreeSet::new();

    let mut set_name = String::new();
    let mut set_description = String::new();
    let mut set_attribution = String::new();

    // the options above, with the usage message headings removed
    let real_long_options = strip_usage_headings(&LONG_OPTIONS);
    let getopt_str = getopt_string(&real_long_options);

    let commandline = format_commandline(&argv);

    let mut g = Getopt::new();
    let mut option_index = 0;
    loop {
        let i = g.getopt_long(
            &mut argv,
            &getopt_str,
            &real_long_options,
            Some(&mut option_index),
        );
        if i == -1 {
            break;
        }
        let optarg = g.optarg.clone().unwrap_or_default();

        match u8::try_from(i) {
            Ok(0) => {}

            Ok(b'o') => out_mbtiles = Some(optarg),

            Ok(b'e') => out_dir = Some(optarg),

            Ok(b'f') => force = 1,

            Ok(b'O') => WANT_OVERZOOM.store(true, Ordering::Relaxed),

            Ok(b'b') => BUFFER.store(atoi(&optarg), Ordering::Relaxed),

            Ok(b'i') => ifmatched = 1,

            Ok(b'A') => set_attribution = optarg,

            Ok(b'n') => set_name = optarg,

            Ok(b'N') => set_description = optarg,

            Ok(b'z') => MAXZOOM.store(atoi(&optarg), Ordering::Relaxed),

            Ok(b'Z') => MINZOOM.store(atoi(&optarg), Ordering::Relaxed),

            Ok(b'J') => filter = Some(read_filter(&optarg)),

            Ok(b'j') => filter = Some(parse_filter(&optarg)),

            Ok(b'p') => {
                if optarg == "k" {
                    PK.store(1, Ordering::Relaxed);
                } else if optarg == "C" {
                    PC.store(1, Ordering::Relaxed);
                } else if optarg == "g" {
                    PG.store(1, Ordering::Relaxed);
                } else if optarg == "e" {
                    PE.store(1, Ordering::Relaxed);
                } else {
                    eprintln!("{}: Unknown option for -p{}", argv[0], optarg);
                    std::process::exit(EXIT_ARGS);
                }
            }

            Ok(b'c') => {
                if csv.is_some() {
                    eprintln!("Only one -c for now");
                    std::process::exit(EXIT_ARGS);
                }

                readcsv(&optarg, &mut header, &mut mapping);
                csv = Some(optarg);
            }

            Ok(b'x') => {
                exclude.insert(optarg);
            }

            Ok(b'X') => EXCLUDE_ALL.store(true, Ordering::Relaxed),

            Ok(b'y') => {
                EXCLUDE_ALL.store(true, Ordering::Relaxed);
                include.insert(optarg);
            }

            Ok(b'l') => {
                keep_layers.insert(optarg);
            }

            Ok(b'L') => {
                remove_layers.insert(optarg);
            }

            Ok(b'R') => match optarg.find(':') {
                None | Some(0) => {
                    eprintln!("{}: -R requires old:new", argv[0]);
                    std::process::exit(EXIT_ARGS);
                }
                Some(cp) => {
                    let before = optarg[..cp].to_string();
                    let after = optarg[cp + 1..].to_string();
                    RENAMES.write().unwrap().entry(before).or_insert(after);
                }
            },

            Ok(b'r') => {
                if let Ok(mut read_file) = File::open(&optarg) {
                    let mut content = Vec::new();
                    let _ = read_file.read_to_end(&mut content);
                    filearg = 1;
                    for sa in getline_lines(&content) {
                        let r = begin_reading(&String::from_utf8_lossy(sa));

                        // put the new tileset_reader in priority order
                        insert_reader(&mut readers, r);
                    }
                }
            }

            Ok(b'q') => QUIET.store(1, Ordering::Relaxed),

            Ok(b'~') => {
                let opt = real_long_options[option_index].name;
                if opt == "tile-stats-attributes-limit" {
                    MAX_TILESTATS_ATTRIBUTES.store(atoi(&optarg) as usize, Ordering::Relaxed);
                } else if opt == "tile-stats-sample-values-limit" {
                    MAX_TILESTATS_SAMPLE_VALUES.store(atoi(&optarg) as usize, Ordering::Relaxed);
                } else if opt == "tile-stats-values-limit" {
                    MAX_TILESTATS_VALUES.store(atoi(&optarg) as usize, Ordering::Relaxed);
                } else if opt == "unidecode-data" {
                    *UNIDECODE_DATA.write().unwrap() = read_unidecode(&optarg);
                } else if opt == "exclude-all-tile-attributes" {
                    EXCLUDE_ALL_TILE_ATTRIBUTES.store(true, Ordering::Relaxed);
                } else if opt == "exclude-all-tile-geometries" {
                    EXCLUDE_ALL_TILE_GEOMETRIES.store(true, Ordering::Relaxed);
                } else {
                    eprintln!("{}: Unrecognized option --{}", argv[0], opt);
                    std::process::exit(EXIT_ARGS);
                }
            }

            _ => usage(&argv),
        }
    }

    let optind = g.optind;
    let minzoom = relaxed(&MINZOOM);
    let maxzoom = relaxed(&MAXZOOM);

    if (argc - optind < 1) && (filearg == 0) {
        usage(&argv);
    }

    if out_mbtiles.is_none() && out_dir.is_none() {
        eprintln!("{}: must specify -o out.mbtiles or -e directory", argv[0]);
        usage(&argv);
    }

    if out_mbtiles.is_some() && out_dir.is_some() {
        eprintln!("{}: Options -o and -e cannot be used together", argv[0]);
        usage(&argv);
    }

    if minzoom > maxzoom {
        eprintln!(
            "{}: Minimum zoom -Z{} cannot be greater than maxzoom -z{}",
            argv[0], minzoom, maxzoom
        );
        std::process::exit(EXIT_ARGS);
    }

    if relaxed(&BUFFER) < 0 {
        eprintln!("{}: buffer cannot be less than 0", argv[0]);
        std::process::exit(EXIT_ARGS);
    }

    if let Some(out_mbtiles) = &out_mbtiles {
        if force != 0 {
            let _ = std::fs::remove_file(out_mbtiles);
        } else if pmtiles_has_suffix(out_mbtiles) {
            check_pmtiles(out_mbtiles, &argv, false);
        }

        outdb = Some(mbtiles_open(out_mbtiles, &argv, 0));
    }
    if let Some(out_dir) = &out_dir {
        check_dir(out_dir, &argv, force != 0, false);
    }

    let mut st = Stats {
        minzoom: i32::MAX,
        minlat: i32::MAX as f64,
        minlon: i32::MAX as f64,
        minlat2: i32::MAX as f64,
        minlon2: i32::MAX as f64,
        maxzoom: i32::MIN,
        maxlat: i32::MIN as f64,
        maxlon: i32::MIN as f64,
        maxlat2: i32::MIN as f64,
        maxlon2: i32::MIN as f64,
        ..Default::default()
    };

    let mut layermap: BTreeMap<String, LayermapEntry> = BTreeMap::new();
    let mut attribution = String::new();
    let mut description = String::new();
    let mut name = String::new();

    if filearg == 0 {
        for arg in &argv[optind..argc] {
            let r = begin_reading(arg);

            // put the new tileset_reader in priority order
            insert_reader(&mut readers, r);
        }
    }

    let mut attribute_descriptions: BTreeMap<String, String> = BTreeMap::new();
    let mut generator_options = String::new();
    let mut strategies: Vec<Strategy> = Vec::new();

    decode(
        readers,
        &mut layermap,
        outdb.as_ref(),
        out_dir.as_deref(),
        &mut st,
        &header,
        &mapping,
        &exclude,
        &include,
        ifmatched,
        &mut attribution,
        &mut description,
        &keep_layers,
        &remove_layers,
        &mut name,
        filter.as_ref().map(|f| f.root()),
        &mut attribute_descriptions,
        &mut generator_options,
        &mut strategies,
    );

    if !set_attribution.is_empty() {
        attribution = set_attribution;
    }
    if !set_description.is_empty() {
        description = set_description;
    }
    if !set_name.is_empty() {
        name = set_name;
    }

    if !generator_options.is_empty() {
        generator_options.push_str("; ");
    }
    generator_options.push_str(&commandline);

    // don't trust the source metadata maxzooms;
    // claim the zooms that were actually written
    st.maxzoom = i32::MIN;
    st.minzoom = i32::MAX;

    for l in layermap.values() {
        if l.minzoom < st.minzoom {
            st.minzoom = l.minzoom;
        }
        if l.maxzoom > st.maxzoom {
            st.maxzoom = l.maxzoom;
        }
    }

    if st.maxlon < st.minlon {
        st.maxlon = 0.0;
        st.minlon = 0.0;
        st.maxlat = 0.0;
        st.minlat = 0.0;
        st.minlon2 = 0.0;
        st.maxlon2 = 0.0;
        st.minlat2 = 0.0;
        st.maxlat2 = 0.0;
    }

    if st.maxlon - st.minlon <= st.maxlon2 - st.minlon2 {
        st.minlon2 = st.minlon;
        st.maxlon2 = st.maxlon;
    }

    let m = make_metadata(
        &name,
        st.minzoom,
        st.maxzoom,
        st.minlat,
        st.minlon,
        st.maxlat,
        st.maxlon,
        st.minlat2,
        st.minlon2,
        st.maxlat2,
        st.maxlon2,
        st.midlat,
        st.midlon,
        if !attribution.is_empty() {
            Some(attribution.as_str())
        } else {
            None
        },
        &layermap,
        true,
        Some(description.as_str()),
        relaxed(&PG) == 0,
        &attribute_descriptions,
        "tile-join",
        &generator_options,
        &strategies,
        st.maxzoom,
        2.5,
        1,
    );

    if let Some(outdb) = &outdb {
        mbtiles_write_metadata(outdb, &m, true);
    } else {
        dir_write_metadata(out_dir.as_deref().unwrap(), &m);
    }

    if let Some(outdb) = outdb.take() {
        mbtiles_close(outdb, &argv[0]);
    }

    drop(filter);

    if let Some(out_mbtiles) = &out_mbtiles
        && pmtiles_has_suffix(out_mbtiles)
    {
        mbtiles_map_image_to_pmtiles(
            out_mbtiles,
            m,
            relaxed(&PC) == 0,
            relaxed(&QUIET) != 0,
            false,
        );
    }
}

/// Restores the default `SIGPIPE` action that a C program starts with, so that
/// writing to a closed pipe ends the program quietly instead of failing the write.
fn restore_sigpipe() {
    // SAFETY: setting a signal to its default action has no preconditions.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

/// `mmap(NULL, size, PROT_READ, MAP_PRIVATE, fd, 0)`. Returns the errno on failure.
fn mmap_file(f: &File, size: u64) -> Result<Mmap, i32> {
    if size == 0 {
        // mmap() refuses an empty mapping
        return Err(libc::EINVAL);
    }
    // SAFETY: the map is only read, and the file is not modified while it is mapped.
    unsafe { Mmap::map(f) }.map_err(|e| e.raw_os_error().unwrap_or(libc::EINVAL))
}

/// `sqlite3_open(fname, &db)`. On failure, returns `sqlite3_errmsg(db)`, without
/// the file name that rusqlite adds to the message when the file can't be opened.
fn sqlite3_open(fname: &str) -> Result<Connection, String> {
    Connection::open_with_flags(
        fname,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )
    .map_err(|e| {
        let msg = e.to_string();
        match e {
            rusqlite::Error::SqliteFailure(err, Some(_))
                if err.code == rusqlite::ErrorCode::CannotOpen =>
            {
                match msg.strip_suffix(&format!(": {}", fname)) {
                    Some(m) => m.to_string(),
                    None => msg,
                }
            }
            _ => msg,
        }
    })
}

/// A prepared statement used through the SQLite C API, as the C++ does, so that
/// the column conversions are SQLite's own and a query can stay open across
/// calls to `advance()`. It must be dropped before its connection is closed.
struct RawStmt {
    stmt: *mut ffi::sqlite3_stmt,
}

impl RawStmt {
    /// `sqlite3_prepare_v2`. On failure, returns `sqlite3_errmsg(db)`.
    fn prepare(db: &Connection, sql: &CStr) -> Result<RawStmt, String> {
        let mut stmt = std::ptr::null_mut();
        // SAFETY: the handle is valid while `db` is borrowed, and `sql` is NUL-terminated.
        let rc = unsafe {
            ffi::sqlite3_prepare_v2(
                db.handle(),
                sql.as_ptr(),
                -1,
                &mut stmt,
                std::ptr::null_mut(),
            )
        };
        if rc != ffi::SQLITE_OK {
            // SAFETY: sqlite3_errmsg returns a NUL-terminated string owned by the connection.
            let msg = unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(db.handle())) };
            return Err(msg.to_string_lossy().into_owned());
        }
        Ok(RawStmt { stmt })
    }

    fn bind_int(&mut self, i: i32, v: i32) {
        // SAFETY: the statement is live until drop.
        unsafe {
            ffi::sqlite3_bind_int(self.stmt, i, v);
        }
    }

    fn step(&mut self) -> i32 {
        // SAFETY: the statement is live until drop.
        unsafe { ffi::sqlite3_step(self.stmt) }
    }

    fn column_int(&self, i: i32) -> i32 {
        // SAFETY: the statement is live until drop.
        unsafe { ffi::sqlite3_column_int(self.stmt, i) }
    }

    /// `sqlite3_column_text`, up to its NUL terminator, or `None` for NULL.
    fn column_text(&self, i: i32) -> Option<&[u8]> {
        // SAFETY: the text stays valid until the next step, which needs `&mut self`.
        unsafe {
            let p = ffi::sqlite3_column_text(self.stmt, i);
            if p.is_null() {
                None
            } else {
                Some(CStr::from_ptr(p as *const c_char).to_bytes())
            }
        }
    }

    /// `std::string(sqlite3_column_blob(), sqlite3_column_bytes())`.
    fn column_blob(&self, i: i32) -> &[u8] {
        // SAFETY: the blob stays valid until the next step, which needs `&mut self`.
        unsafe {
            let p = ffi::sqlite3_column_blob(self.stmt, i);
            let len = ffi::sqlite3_column_bytes(self.stmt, i);
            if p.is_null() {
                &[]
            } else {
                std::slice::from_raw_parts(p as *const u8, len as usize)
            }
        }
    }
}

impl Drop for RawStmt {
    fn drop(&mut self) {
        // SAFETY: the statement was prepared and is finalized once.
        unsafe {
            ffi::sqlite3_finalize(self.stmt);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilecmp_pulls_from_the_end_in_tms_order() {
        let mut v = vec![(0, 0), (1, 1), (0, 1), (1, 0)];
        v.sort_by(tilecmp);
        // x descending, then y ascending, so that popping from the end
        // gives x ascending, then y descending (TMS y ascending)
        assert_eq!(v, vec![(1, 0), (1, 1), (0, 0), (0, 1)]);
    }

    #[test]
    fn getline_lines_like_getline() {
        assert_eq!(getline_lines(b"a\nb\n"), vec![&b"a"[..], b"b"]);
        assert_eq!(getline_lines(b"a\nb"), vec![&b"a"[..], b"b"]);
        assert_eq!(getline_lines(b""), Vec::<&[u8]>::new());
        assert_eq!(getline_lines(b"\n"), vec![&b""[..]]);
    }

    #[test]
    fn scan_center() {
        let (mut lon, mut lat) = (1.0, 2.0);
        scan_lf_comma_lf("-122.3,37.8,10", &mut lon, &mut lat);
        assert_eq!((lon, lat), (-122.3, 37.8));
        let (mut lon, mut lat) = (1.0, 2.0);
        scan_lf_comma_lf("5;6", &mut lon, &mut lat);
        assert_eq!((lon, lat), (5.0, 2.0));
    }
}
