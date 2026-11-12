//! `manby join`: tippecanoe's tile-join, making the same tiles faster. See
//! DESIGN.md.
//!
//! [`join`] takes [`JoinOptions`] and writes the joined tileset. The order in
//! which input tiles are combined and the metadata are tile-join's; the work
//! on the tiles themselves runs on all threads, and the output goes to the
//! same sinks as manby's tiling, PMTiles straight from the tiles.

pub mod args;
pub mod csv;
#[doc(hidden)]
pub mod key;
#[doc(hidden)]
pub mod merge;
#[doc(hidden)]
pub mod overzoom;
#[doc(hidden)]
pub mod source;
#[doc(hidden)]
pub mod stats;
#[doc(hidden)]
pub mod tile;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use tippecanoe::errors::{EXIT_CSV, EXIT_EXISTS};
use tippecanoe::jsonpull::{JsonPull, JsonRef, JsonType};
use tippecanoe::mbtiles::{LayermapEntry, make_metadata, merge_layermaps};
use tippecanoe::tile::Strategy;

pub use key::TieBreak;

use crate::options::Output;
use crate::sink::{self, SinkSettings};
use crate::{Error, Result};
use merge::{Group, Queue, Reader, Settings};
use source::Source;
use stats::{Accum, std_max, std_min};
use tile::{Rules, Scratch};

/// tile-join's limit on the compressed size of a tile, which `-pk` lifts.
pub const MAX_TILE_BYTES: usize = 500_000;

/// Output tiles worked on together.
const BATCH: usize = 2048;

/// Everything tile-join's command line says. [`JoinOptions::default`] has
/// tile-join's defaults, and [`args::parse_args`] reads them from a command
/// line.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct JoinOptions {
    /// The tilesets to join, in order.
    pub inputs: Vec<String>,
    /// `-r`: the tilesets the listed files name, one per line, which tile-join
    /// opens before the output and uses instead of `inputs`.
    pub read_from: Option<Vec<String>>,
    /// `-o`, `--output` or `-e`, `--output-to-directory`
    pub output: Output,
    /// `-f`, `--force`: replaces an existing tileset
    pub force: bool,
    /// `-n`, `--name`; empty means not given
    pub name: String,
    /// `-N`, `--description`; empty means not given
    pub description: String,
    /// `-A`, `--attribution`; empty means not given
    pub attribution: String,
    /// `-Z`, `--minimum-zoom`
    pub minzoom: i32,
    /// `-z`, `--maximum-zoom`
    pub maxzoom: i32,
    /// `-O`, `--overzoom`
    pub overzoom: bool,
    /// `-b`, `--buffer`, for `--overzoom`
    pub buffer: i32,
    /// `-l`, `--layer`
    pub keep_layers: BTreeSet<String>,
    /// `-L`, `--exclude-layer`
    pub remove_layers: BTreeSet<String>,
    /// `-R`, `--rename-layer`
    pub renames: BTreeMap<String, String>,
    /// `-c`, as read
    pub csv: Option<Arc<csv::RawCsv>>,
    /// `-i`
    pub ifmatched: bool,
    /// `--empty-csv-columns-are-null` (`-pe`)
    pub empty_csv_columns_are_null: bool,
    /// `-x`, `--exclude`
    pub exclude: BTreeSet<String>,
    /// `-y`, `--include`
    pub include: BTreeSet<String>,
    /// `-X`, `--exclude-all`
    pub exclude_all: bool,
    /// `--exclude-all-tile-attributes`
    pub exclude_all_tile_attributes: bool,
    /// `--exclude-all-tile-geometries`
    pub exclude_all_tile_geometries: bool,
    /// `-j` or `-J`
    pub filter: Option<crate::filter::Filter>,
    /// `-pk`, `--no-tile-size-limit`
    pub no_tile_size_limit: bool,
    /// `-pC`, `--no-tile-compression`
    pub no_tile_compression: bool,
    /// `-pg`, `--no-tile-stats`
    pub no_tile_stats: bool,
    /// `--tile-stats-attributes-limit`
    pub max_tilestats_attributes: usize,
    /// `--tile-stats-sample-values-limit`
    pub max_tilestats_sample_values: usize,
    /// `--tile-stats-values-limit`
    pub max_tilestats_values: usize,
    /// `-q`, `--quiet`
    pub quiet: bool,
    /// `--unidecode-data`
    pub unidecode_data: Vec<String>,
    /// Worker threads; 0 means one per available CPU.
    pub threads: usize,
    /// `--compression=zlib`: store every tile as zlib compresses it, as
    /// tile-join does. Otherwise tiles that fit whatever zlib does are
    /// compressed with libdeflate.
    pub zlib_tiles: bool,
    /// How input tiles at the same z/x/y are ordered (see `key.rs`).
    pub tie_break: TieBreak,
    /// `argv[0]`, for messages.
    pub program: String,
    /// The command line, for the metadata's `generator_options`.
    pub commandline: String,
    /// Prints how long the parts of the join take.
    pub timing: bool,
}

impl Default for JoinOptions {
    fn default() -> Self {
        JoinOptions {
            inputs: Vec::new(),
            read_from: None,
            output: Output::None,
            force: false,
            name: String::new(),
            description: String::new(),
            attribution: String::new(),
            minzoom: 0,
            maxzoom: 32,
            overzoom: false,
            buffer: 5,
            keep_layers: BTreeSet::new(),
            remove_layers: BTreeSet::new(),
            renames: BTreeMap::new(),
            csv: None,
            ifmatched: false,
            empty_csv_columns_are_null: false,
            exclude: BTreeSet::new(),
            include: BTreeSet::new(),
            exclude_all: false,
            exclude_all_tile_attributes: false,
            exclude_all_tile_geometries: false,
            filter: None,
            no_tile_size_limit: false,
            no_tile_compression: false,
            no_tile_stats: false,
            max_tilestats_attributes: 1000,
            max_tilestats_sample_values: 1000,
            max_tilestats_values: 100,
            quiet: false,
            unidecode_data: Vec::new(),
            threads: 0,
            zlib_tiles: false,
            tie_break: TieBreak::default(),
            program: "tile-join".to_string(),
            commandline: String::new(),
            timing: false,
        }
    }
}

/// What a join wrote.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct JoinSummary {
    /// Tiles written.
    pub tiles: u64,
    /// Tiles left out for being over the size limit.
    pub skipped: u64,
    /// The lowest zoom written.
    pub minzoom: i32,
    /// The highest zoom written.
    pub maxzoom: i32,
}

/// The bounds and zooms tile-join keeps in its `stats`.
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

/// Joins the tilesets as `o` says.
pub fn join(o: &JoinOptions) -> Result<JoinSummary> {
    let threads = crate::pool::threads(o.threads);
    let pool = crate::pool::Pool::new(threads)?;
    pool.install(|| join_in_pool(o, threads))
}

fn set_tippecanoe_globals(o: &JoinOptions) {
    use std::sync::atomic::Ordering::Relaxed;
    use tippecanoe::globals as g;
    // TRANSITIONAL: tile-stats and the overzooming code borrowed from
    // tippecanoe-rs read its globals, which a tile-join starts with at their
    // defaults, apart from these.
    crate::options::set_tippecanoe_globals(&crate::Options::default(), 0);
    g::MAX_TILESTATS_ATTRIBUTES.store(o.max_tilestats_attributes, Relaxed);
    g::MAX_TILESTATS_SAMPLE_VALUES.store(o.max_tilestats_sample_values, Relaxed);
    g::MAX_TILESTATS_VALUES.store(o.max_tilestats_values, Relaxed);
}

/// Opens the output as tile-join does, with its messages.
fn open_output(o: &JoinOptions) -> Result<Box<dyn sink::TileSink>> {
    let program = &o.program;
    let settings = SinkSettings {
        force: o.force,
        allow_existing: false,
        tile_compression: !o.no_tile_compression,
        tmpdir: std::env::temp_dir(),
        memory_budget: 16 << 20,
        program: program.clone(),
        ..Default::default()
    };
    match &o.output {
        Output::Pmtiles(path) if !o.force && std::fs::metadata(path).is_ok() => Err(Error::new(
            EXIT_EXISTS,
            format!(
                "{0}: Tileset \"{1}\" already exists. You can use --force if you want to delete the old tileset.\n{0}: {1}: file exists",
                program, path
            ),
        )),
        out => sink::open(out, &settings),
    }
}

fn join_in_pool(o: &JoinOptions, threads: usize) -> Result<JoinSummary> {
    set_tippecanoe_globals(o);
    let settings = Settings {
        want_overzoom: o.overzoom,
        buffer: o.buffer,
        tie_break: o.tie_break,
    };

    // tile-join opens the -r inputs while it reads its options, and the
    // others once the output is open.
    let open = |names: &[String]| -> Result<Vec<Source>> {
        names
            .iter()
            .map(|name| Source::open(name, o.minzoom, o.maxzoom))
            .collect()
    };
    let (sources, sink) = match &o.read_from {
        Some(list) => {
            let sources = open(list)?;
            (sources, open_output(o)?)
        }
        None => {
            let sink = open_output(o)?;
            (open(&o.inputs)?, sink)
        }
    };

    let mut queue = Queue::new();
    for (i, s) in sources.iter().enumerate() {
        let r = Reader::begin(s, i, &settings)?;
        queue.add(r, &settings)?;
    }

    let rules = rules(o)?;
    let accums: Vec<Mutex<Accum>> = (0..threads + 1)
        .map(|_| Mutex::new(Accum::new(o.max_tilestats_sample_values)))
        .collect();
    let scratches: Vec<Mutex<Scratch>> = (0..threads + 1).map(|_| Mutex::default()).collect();
    let written = std::sync::atomic::AtomicU64::new(0);
    let skipped = std::sync::atomic::AtomicU64::new(0);

    let started = std::time::Instant::now();
    let (mut queueing, mut working) = (0.0, 0.0);
    let mut groups: Vec<Group> = Vec::with_capacity(BATCH);
    loop {
        let t = std::time::Instant::now();
        groups.clear();
        let more = queue.run(&settings, o.minzoom, o.maxzoom, BATCH, &mut groups)?;
        queueing += t.elapsed().as_secs_f64();
        let t = std::time::Instant::now();
        if let Some(g) = groups.first()
            && !o.quiet
        {
            eprint!("{}/{}/{}  \r", g.z, g.x, g.y);
        }
        // a mapping of each archive for this batch only, so that the pages
        // of tiles already done leave memory
        let maps: Vec<Option<memmap2::Mmap>> = sources.iter().map(|s| s.batch_map()).collect();
        // The biggest tiles first, each thread taking the next when it is
        // done, so that no thread is left with a big tile at the end.
        let mut order: Vec<(u64, u32)> = groups
            .iter()
            .enumerate()
            .map(|(i, g)| (g.members.iter().map(|m| m.1.size()).sum(), i as u32))
            .collect();
        order.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let next = std::sync::atomic::AtomicUsize::new(0);
        let failure: Mutex<Option<Error>> = Mutex::new(None);
        let work = || {
            let slot = rayon::current_thread_index().unwrap_or(threads);
            let mut acc = accums[slot].lock().unwrap_or_else(|p| p.into_inner());
            let mut scratch = scratches[slot].lock().unwrap_or_else(|p| p.into_inner());
            loop {
                let k = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if k >= order.len() {
                    break;
                }
                let g = &groups[order[k].1 as usize];
                let done = make_tile(g, &sources, &maps, &rules, o, &mut acc, &mut scratch)
                    .and_then(|tile| match tile {
                        Made::Tile(data) => {
                            sink.write_tile(g.z as u8, g.x as u32, g.y as u32, &data)?;
                            written.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            Ok(())
                        }
                        Made::TooBig(n) => {
                            eprintln!(
                                "Tile {}/{}/{} size is {}, >{}. Skipping this tile.",
                                g.z, g.x, g.y, n, MAX_TILE_BYTES
                            );
                            skipped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            Ok(())
                        }
                        Made::Nothing => Ok(()),
                    });
                if let Err(e) = done {
                    let mut f = failure.lock().unwrap_or_else(|p| p.into_inner());
                    f.get_or_insert(e);
                    // the others stop at their next tile
                    next.store(order.len(), std::sync::atomic::Ordering::Relaxed);
                    break;
                }
            }
        };
        rayon::scope(|s| {
            for _ in 0..threads {
                s.spawn(|_| work());
            }
        });
        if let Some(e) = failure.into_inner().unwrap_or_else(|p| p.into_inner()) {
            return Err(e);
        }
        working += t.elapsed().as_secs_f64();
        if !more {
            break;
        }
    }
    if o.timing {
        eprintln!("readers {:.3}s, tiles {:.3}s", queueing, working);
    }

    // The threads' layers and bounds, combined as tile-join combines its threads'.
    let accums: Vec<Accum> = accums
        .into_iter()
        .map(|a| a.into_inner().unwrap_or_else(|p| p.into_inner()))
        .collect();
    let layermaps: Vec<BTreeMap<String, LayermapEntry>> =
        accums.iter().map(|a| a.layermap()).collect();
    let mut layermap = merge_layermaps(&layermaps);
    let mut bounds = Accum::new(0);
    for a in &accums {
        bounds.merge_bounds(a);
    }

    let mut st = Stats {
        minzoom: i32::MAX,
        maxzoom: i32::MIN,
        midlat: 0.0,
        midlon: 0.0,
        minlat: i32::MAX as f64,
        minlon: i32::MAX as f64,
        maxlat: i32::MIN as f64,
        maxlon: i32::MIN as f64,
        minlat2: i32::MAX as f64,
        minlon2: i32::MAX as f64,
        maxlat2: i32::MIN as f64,
        maxlon2: i32::MIN as f64,
    };
    st.minlon = std_min(bounds.minlon, st.minlon);
    st.maxlon = std_max(bounds.maxlon, st.maxlon);
    st.minlat = std_min(bounds.minlat, st.minlat);
    st.maxlat = std_max(bounds.maxlat, st.maxlat);
    st.minlon2 = std_min(bounds.minlon2, st.minlon2);
    st.maxlon2 = std_max(bounds.maxlon2, st.maxlon2);
    st.minlat2 = std_min(bounds.minlat, st.minlat2);
    st.maxlat2 = std_max(bounds.maxlat, st.maxlat2);

    let mut info = SourceInfo::default();
    for &r in &queue.order {
        read_metadata(
            queue.readers[r].source,
            o,
            &mut st,
            &mut layermap,
            &mut info,
        )?;
    }
    drop(queue);

    if !o.attribution.is_empty() {
        info.attribution = o.attribution.clone();
    }
    if !o.description.is_empty() {
        info.description = o.description.clone();
    }
    if !o.name.is_empty() {
        info.name = o.name.clone();
    }
    if !info.generator_options.is_empty() {
        info.generator_options.push_str("; ");
    }
    info.generator_options.push_str(&o.commandline);

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
        &info.name,
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
        (!info.attribution.is_empty()).then_some(info.attribution.as_str()),
        &layermap,
        true,
        Some(info.description.as_str()),
        !o.no_tile_stats,
        &info.attribute_descriptions,
        "tile-join",
        &info.generator_options,
        &info.strategies,
        st.maxzoom,
        2.5,
        1,
    );
    let t = std::time::Instant::now();
    sink.finish(&m)?;
    if o.timing {
        eprintln!(
            "output {:.3}s, {:.3}s in all",
            t.elapsed().as_secs_f64(),
            started.elapsed().as_secs_f64()
        );
    }

    Ok(JoinSummary {
        tiles: written.into_inner(),
        skipped: skipped.into_inner(),
        minzoom: st.minzoom,
        maxzoom: st.maxzoom,
    })
}

fn rules(o: &JoinOptions) -> Result<Rules> {
    let csv = match &o.csv {
        Some(raw) => raw.prepare(o.empty_csv_columns_are_null),
        None => csv::Csv::default(),
    };
    Ok(Rules {
        header: csv.header,
        mapping: csv.mapping,
        exclude: o.exclude.clone(),
        include: o.include.clone(),
        keep_layers: o.keep_layers.clone(),
        remove_layers: o.remove_layers.clone(),
        renames: o.renames.clone(),
        ifmatched: o.ifmatched,
        exclude_all: o.exclude_all,
        exclude_all_tile_attributes: o.exclude_all_tile_attributes,
        exclude_all_tile_geometries: o.exclude_all_tile_geometries,
        filter: o.filter.clone(),
        unidecode_data: o.unidecode_data.clone(),
    })
}

enum Made {
    Tile(Vec<u8>),
    TooBig(usize),
    Nothing,
}

/// The output tile of a group: tile-join's `join_worker` for one tile.
fn make_tile(
    g: &Group,
    sources: &[Source],
    maps: &[Option<memmap2::Mmap>],
    rules: &Rules,
    o: &JoinOptions,
    acc: &mut Accum,
    scratch: &mut Scratch,
) -> Result<Made> {
    let mut data = Vec::with_capacity(g.members.len());
    for (i, t) in &g.members {
        data.push(sources[*i].data_in(t, maps[*i].as_ref())?);
    }
    let inputs: Vec<&[u8]> = data.iter().map(|d| &d[..]).collect();
    let mut buffers = std::mem::take(&mut scratch.buffers);
    if buffers.len() < inputs.len() {
        buffers.resize_with(inputs.len(), Vec::new);
    }
    let mut decompressor = scratch.decompressor.take().unwrap_or_default();
    let (z, x, y) = (g.z as i32, g.x as u32, g.y as u32);
    let result = (|| {
        let decoded = tile::decode_all(&inputs, &mut buffers, &mut decompressor, z, x, y)?;
        let mut pbf = Vec::new();
        let anything = tile::join_tile(&decoded, z, x, y, rules, acc, scratch, &mut pbf)?;
        if !anything {
            return Ok(Made::Nothing);
        }
        if inputs.len() == 1
            && !o.no_tile_compression
            && !o.zlib_tiles
            && (o.no_tile_size_limit || crate::compress::zlib_bound(pbf.len()) <= MAX_TILE_BYTES)
            && is_gzip_of(inputs[0], &pbf)
        {
            // The tile is the input tile, which is kept whatever zlib would
            // make of it, so the input's own compression of it will do.
            return Ok(Made::Tile(inputs[0].to_vec()));
        }
        let (tile, keep) = if o.no_tile_compression {
            let n = pbf.len();
            (pbf, o.no_tile_size_limit || n <= MAX_TILE_BYTES)
        } else if !o.zlib_tiles
            && (o.no_tile_size_limit || crate::compress::zlib_bound(pbf.len()) <= MAX_TILE_BYTES)
        {
            // kept whatever zlib would make of it, so libdeflate's is as good
            (crate::compress::gzip(&pbf, true), true)
        } else {
            let z = crate::compress::gzip(&pbf, false);
            let keep = o.no_tile_size_limit || z.len() <= MAX_TILE_BYTES;
            (z, keep)
        };
        Ok(if keep {
            Made::Tile(tile)
        } else {
            Made::TooBig(tile.len())
        })
    })();
    scratch.buffers = buffers;
    scratch.decompressor = Some(decompressor);
    result
}

/// Whether `data` is a gzip stream of exactly `content`, with nothing after
/// it, that zlib or libdeflate made, as their headers say. Only those are
/// stored as they are: the tileset will say tile-join made it, and a later
/// join orders tiles at the same z/x/y by their bytes, which must then be
/// zlib's own, or libdeflate's, which compare as zlib's (see `key.rs`).
/// A stream from any other compressor is compressed again.
pub(crate) fn is_gzip_of(data: &[u8], content: &[u8]) -> bool {
    let n = data.len();
    n >= 18
        && (data[..10] == *key::zlib_header() || data[..10] == key::LIBDEFLATE_HEADER)
        && data[n - 4..] == (content.len() as u32).to_le_bytes()
        && data[n - 8..n - 4] == libdeflater::crc32(content).to_le_bytes()
}

/// What tile-join reads from the inputs' metadata.
#[derive(Default)]
struct SourceInfo {
    attribution: String,
    description: String,
    name: String,
    attribute_descriptions: BTreeMap<String, String>,
    generator_options: String,
    strategies: Vec<Strategy>,
}

/// What tile-join reads from one input's metadata when it closes its reader.
fn read_metadata(
    source: &Source,
    o: &JoinOptions,
    st: &mut Stats,
    layermap: &mut BTreeMap<String, LayermapEntry>,
    info: &mut SourceInfo,
) -> Result<()> {
    use source::{metadata_int, metadata_text};
    let db = source.metadata_db()?;
    let db = db.conn();

    if let Some(v) = metadata_int(db, c"SELECT value from metadata where name = 'minzoom'") {
        let minz = v.max(o.minzoom);
        st.minzoom = st.minzoom.min(minz);
    }
    if let Some(v) = metadata_int(db, c"SELECT value from metadata where name = 'maxzoom'") {
        let maxz = v.min(o.maxzoom);
        if !o.overzoom && st.maxzoom >= 0 && maxz != st.maxzoom {
            eprintln!(
                "Warning: mismatched maxzooms: {} in {} vs previous {}",
                maxz, source.name, st.maxzoom
            );
        }
        st.maxzoom = st.maxzoom.max(maxz);
    }
    let text = |sql: &std::ffi::CStr| {
        metadata_text(db, sql).map(|s| String::from_utf8_lossy(&s).into_owned())
    };
    if let Some(s) = text(c"SELECT value from metadata where name = 'center'") {
        scan_lf_comma_lf(&s, &mut st.midlon, &mut st.midlat);
    }
    if let Some(s) = text(c"SELECT value from metadata where name = 'attribution'") {
        info.attribution = s;
    }
    if let Some(s) = text(c"SELECT value from metadata where name = 'description'") {
        info.description = s;
    }
    if let Some(s) = text(c"SELECT value from metadata where name = 'name'") {
        if info.name.is_empty() {
            info.name = s;
        } else {
            let proposed = format!("{} + {}", info.name, s);
            if proposed.len() < 255 {
                info.name = proposed;
            }
        }
    }
    if let Some(s) = metadata_text(db, c"SELECT value from metadata where name = 'json'") {
        let mut jp = JsonPull::begin_string(&s);
        if let Some(t) = jp.read_tree()
            && t.root().type_() == JsonType::Hash
        {
            handle_vector_layers(
                t.root().hash_get("vector_layers"),
                layermap,
                &mut info.attribute_descriptions,
            );
        }
    }
    if let Some(s) = text(c"SELECT value from metadata where name = 'generator_options'") {
        if !info.generator_options.is_empty() {
            info.generator_options.push_str("; ");
            info.generator_options.push_str(&s);
        } else {
            info.generator_options = s;
        }
    }
    if let Some(mut stmt) =
        source::RawStmt::prepare(db, c"SELECT value from metadata where name = 'strategies'").ok()
        && stmt.step() == rusqlite::ffi::SQLITE_ROW
    {
        handle_strategies(stmt.column_text(0), &mut info.strategies);
    }
    Ok(())
}

/// `sscanf(s, "%lf,%lf", a, b)`, which assigns as many values as it reads.
fn scan_lf_comma_lf(s: &str, a: &mut f64, b: &mut f64) {
    let (v, n) = tippecanoe::cstd::strtod(s);
    if n == 0 {
        return;
    }
    *a = v;
    let Some(rest) = s[n..].strip_prefix(',') else {
        return;
    };
    let (v, n) = tippecanoe::cstd::strtod(rest);
    if n == 0 {
        return;
    }
    *b = v;
}

fn handle_strategies(s: Option<&[u8]>, st: &mut Vec<Strategy>) {
    let s = s.unwrap_or(b"");
    let ss = String::from_utf8_lossy(s);
    let mut jp = JsonPull::begin_string(s);
    let Some(o) = jp.read_tree() else {
        return;
    };
    let o = o.root();
    if o.type_() != JsonType::Array {
        return;
    }
    for (i, h) in o.array().iter().enumerate() {
        if h.type_() != JsonType::Hash {
            eprintln!("Element {} is not a hash: {}", i, ss);
            continue;
        }
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
        if layer.type_() != JsonType::Hash {
            continue;
        }
        if let (Some(id), Some(desc)) = (layer.hash_get("id"), layer.hash_get("description"))
            && id.type_() == JsonType::String
            && desc.type_() == JsonType::String
        {
            let sdesc = desc.string();
            if !sdesc.is_empty()
                && let Some(f) = layermap.get_mut(id.string())
            {
                f.description = sdesc.to_string();
            }
        }
        if let Some(fields) = layer.hash_get("fields")
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

/// tile-join's message for a CSV row with more columns than the header,
/// which it cannot join.
pub(crate) fn csv_row_too_long(key: &str) -> Error {
    Error::new(
        EXIT_CSV,
        format!("CSV row {} has more columns than the header", key),
    )
}
