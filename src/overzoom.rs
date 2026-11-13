//! `manby overzoom`, tippecanoe-overzoom: copies of tippecanoe-rs's
//! overzoom.rs and of the `overzoom_source_tiles` it runs, from clip.rs, with
//! manby's feature filters (`crate::filter`), so that `like`, `ilike` and
//! `intersects` work in them. `intersects` is tested against each feature's
//! geometry as clipped to the new tile.
//!
//! The command line, messages and tiles are tippecanoe-rs's. Left out are the
//! arguments of `overzoom_source_tiles` that tippecanoe-overzoom does not use:
//! the child tiles for tile-join, bins, clipping boxes and a feature limit.
//! Polygons are cleaned by way of manby's copy of the cleaning
//! (`crate::clean`), as in tile-join's overzooming.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::AtomicI32;

use tippecanoe::attribute::{AttributeOp, preserve_attribute_mvt, set_attribute_accum_arg};
use tippecanoe::clip::{
    clip_lines, clip_point, close_poly, douglas_peucker, reduce_tiny_poly, remove_noop,
    simple_clip_poly, to_tile_scale,
};
use tippecanoe::cstd::{atof, atoi, strtoll};
use tippecanoe::errors::{EXIT_FAILURE, EXIT_JSON, EXIT_MVT, EXIT_OPEN, EXIT_PROTOBUF};
use tippecanoe::geometry::{
    Draw, DrawVec, SourceTile, VT_LINE, VT_LINETO, VT_MOVETO, VT_POINT, VT_POLYGON,
};
use tippecanoe::getopt::{Getopt, LongOption, NO_ARGUMENT, REQUIRED_ARGUMENT};
use tippecanoe::jsonpull::JsonPull;
use tippecanoe::mvt::{
    MVT_BOOL, MVT_CLOSEPATH, MVT_LINETO, MVT_MOVETO, MvtFeature, MvtGeometry, MvtLayer, MvtTile,
    MvtValue, compress, mvt_value_to_long_long,
};
use tippecanoe::serial::KeyPool;
use tippecanoe::text::{read_unidecode, starts_with};
use tippecanoe::usage::{UsageRequiredOption, getopt_string, print_usage, strip_usage_headings};

use crate::clean::clean_or_clip_poly;
use crate::filter::{Filter, FilterGeometry};
use crate::options::os_message;
use crate::{Error, Result};

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

static LONG_OPTIONS: [LongOption; 25] = [
    opt("Output tile", 0, None, 0),
    opt("output", REQUIRED_ARGUMENT, None, b'o'),
    opt("source-tile", REQUIRED_ARGUMENT, None, b't'),
    opt("no-tile-compression", NO_ARGUMENT, None, b'd' & 0x1F),
    //
    opt("Tile resolution", 0, None, 0),
    opt("full-detail", REQUIRED_ARGUMENT, None, b'd'),
    opt("buffer", REQUIRED_ARGUMENT, None, b'b'),
    //
    opt("Filtering feature attributes", 0, None, 0),
    opt("include", REQUIRED_ARGUMENT, None, b'y'),
    opt("exclude", REQUIRED_ARGUMENT, None, b'x'),
    opt("exclude-prefix", REQUIRED_ARGUMENT, None, b'x' & 0x1F),
    //
    opt("Modifying feature attributes", 0, None, 0),
    opt("accumulate-attribute", REQUIRED_ARGUMENT, None, b'E'),
    //
    opt("Filtering features", 0, None, 0),
    opt("feature-filter", REQUIRED_ARGUMENT, None, b'j'),
    opt("feature-filter-file", REQUIRED_ARGUMENT, None, b'J'),
    opt("filter-points-multiplier", NO_ARGUMENT, None, b'm'),
    opt("deduplicate-by-id", NO_ARGUMENT, None, b'i' & 0x1F),
    //
    opt("Line and polygon simplification", 0, None, 0),
    opt("line-simplification", REQUIRED_ARGUMENT, None, b'S'),
    opt("tiny-polygon-size", REQUIRED_ARGUMENT, None, b's' & 0x1F),
    //
    opt("Reordering features within the tile", 0, None, 0),
    opt("preserve-input-order", NO_ARGUMENT, None, b'o' & 0x1F),
    //
    opt("", 0, None, 0),
    opt("unidecode-data", REQUIRED_ARGUMENT, None, b'u' & 0x1F),
];

/// The usage message, with what comes before it, as an error.
fn usage(argv0: &str, before: &str) -> Error {
    let forms = [
        "[options] tile.pbf.gz oz/ox/oy nz/nx/ny",
        "[options] --source-tile=nz/nx/ny tile.pbf.gz oz/ox/oy ...",
    ];
    let required = [UsageRequiredOption {
        name: "output",
        placeholder: "newtile.pbf.gz",
        alternation: 0,
    }];

    let mut out = before.as_bytes().to_vec();
    print_usage(&mut out, argv0, &forms, &LONG_OPTIONS, &required);
    out.extend_from_slice(
        b"\nThe tile nz/nx/ny is created from the tile or tiles oz/ox/oy that contain it.\n",
    );
    out.extend_from_slice(
        b"In the second form, each source tile is named by a file name and a z/x/y pair.",
    );
    Error::new(EXIT_FAILURE, String::from_utf8_lossy(&out).into_owned())
}

/// `sscanf(s, "%d/%d/%d", &z, &x, &y) == 3`.
fn scan_zxy(s: &str) -> Option<(i32, i32, i32)> {
    let mut out = [0i32; 3];
    let mut rest = s;
    for (i, v) in out.iter_mut().enumerate() {
        if i > 0 {
            rest = rest.strip_prefix('/')?;
        }
        let (n, used) = strtoll(rest, 10);
        if used == 0 {
            return None;
        }
        *v = n as i32;
        rest = &rest[used..];
    }
    Some((out[0], out[1], out[2]))
}

/// tippecanoe-overzoom's settings.
#[derive(Clone, Debug, Default)]
pub struct OverzoomOptions {
    pub output: String,
    /// The tile files, each with the tile it is.
    pub sources: Vec<(String, i32, i32, i32)>,
    /// The tile to make.
    pub nz: i32,
    pub nx: i32,
    pub ny: i32,
    /// tippecanoe-style: mvt extent == 1 << detail, or the source's if <= 0
    pub detail: i32,
    /// tippecanoe-style: mvt buffer == extent * buffer / 256
    pub buffer: i32,
    pub keep: BTreeSet<String>,
    pub exclude: BTreeSet<String>,
    pub exclude_prefix: Vec<String>,
    pub do_compress: bool,
    pub demultiply: bool,
    /// The `-j` or `-J` filter's text, if any.
    pub filter: Option<String>,
    pub preserve_input_order: bool,
    pub attribute_accum: HashMap<String, AttributeOp>,
    pub unidecode_data: Vec<String>,
    pub simplification: f64,
    pub tiny_polygon_size: f64,
    pub deduplicate_by_id: bool,
}

/// tippecanoe-overzoom's command line.
pub fn parse_args(argv: &[String]) -> Result<OverzoomOptions> {
    let mut argv = argv.to_vec();
    let argc = argv.len();
    let mut outtile: Option<String> = None;
    let mut outfile: Option<String> = None;
    let mut filter: Option<String> = None;
    let mut o = OverzoomOptions {
        detail: 12,
        buffer: 5,
        do_compress: true,
        ..Default::default()
    };

    // the options above, with the usage message headings removed
    let real_long_options = strip_usage_headings(&LONG_OPTIONS);
    let getopt_str = getopt_string(&real_long_options);

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

        const NO_TILE_COMPRESSION: u8 = b'd' & 0x1F;
        const EXCLUDE_PREFIX: u8 = b'x' & 0x1F;
        const PRESERVE_INPUT_ORDER: u8 = b'o' & 0x1F;
        const UNIDECODE_DATA: u8 = b'u' & 0x1F;
        const TINY_POLYGON_SIZE: u8 = b's' & 0x1F;
        const DEDUPLICATE_BY_ID: u8 = b'i' & 0x1F;

        match u8::try_from(i) {
            Ok(b'y') => {
                o.keep.insert(optarg);
            }
            Ok(b'x') => {
                o.exclude.insert(optarg);
            }
            Ok(EXCLUDE_PREFIX) => o.exclude_prefix.push(optarg),
            Ok(b'o') => outfile = Some(optarg),
            Ok(b'd') => o.detail = atoi(&optarg),
            Ok(b'b') => o.buffer = atoi(&optarg),
            Ok(b'm') => o.demultiply = true,
            Ok(b'j') => filter = Some(optarg),
            Ok(b'J') => {
                let data = std::fs::read(&optarg).map_err(|e| {
                    Error::new(EXIT_OPEN, format!("{}: {}", optarg, os_message(&e)))
                })?;
                filter = Some(String::from_utf8_lossy(&data).into_owned());
            }
            Ok(PRESERVE_INPUT_ORDER) => o.preserve_input_order = true,
            Ok(b'E') => set_attribute_accum_arg(&mut o.attribute_accum, &optarg, &argv),
            Ok(UNIDECODE_DATA) => o.unidecode_data = read_unidecode(&optarg),
            Ok(b't') => outtile = Some(optarg),
            Ok(TINY_POLYGON_SIZE) => o.tiny_polygon_size = atof(&optarg),
            Ok(b'S') => o.simplification = atof(&optarg),
            Ok(NO_TILE_COMPRESSION) => o.do_compress = false,
            Ok(DEDUPLICATE_BY_ID) => o.deduplicate_by_id = true,
            _ => {
                let mut msg = b"Unrecognized flag -".to_vec();
                msg.push(i as u8);
                msg.push(b'\n');
                return Err(usage(&argv[0], &String::from_utf8_lossy(&msg)));
            }
        }
    }
    // an empty filter is none
    o.filter = filter.filter(|f| !f.is_empty());

    let optind = g.optind;
    o.output = outfile.ok_or_else(|| {
        usage(
            &argv[0],
            &format!("{}: must specify -o newtile.pbf.gz\n", argv[0]),
        )
    })?;
    let zxy = |s: &str| {
        scan_zxy(s).ok_or_else(|| usage(&argv[0], &format!("{}: not in z/x/y form\n", s)))
    };

    match outtile {
        None => {
            // single input
            if argc - optind != 3 {
                return Err(usage(&argv[0], "Wrong number of arguments\n"));
            }
            let (oz, ox, oy) = zxy(&argv[optind + 1])?;
            (o.nz, o.nx, o.ny) = zxy(&argv[optind + 2])?;
            o.sources.push((argv[optind].clone(), oz, ox, oy));
        }
        Some(outtile) => {
            // multiple inputs
            if !(argc - optind).is_multiple_of(2) {
                return Err(usage(&argv[0], "Unpaired arguments\n"));
            }
            (o.nz, o.nx, o.ny) = zxy(&outtile)?;
            let mut i = optind;
            while i + 1 < argc {
                let (oz, ox, oy) = zxy(&argv[i + 1])?;
                o.sources.push((argv[i].clone(), oz, ox, oy));
                i += 2;
            }
        }
    }
    Ok(o)
}

/// Reads the source tiles, overzooms them and writes the new tile, as
/// tippecanoe-overzoom does.
pub fn run(o: &OverzoomOptions) -> Result<()> {
    let filter = match &o.filter {
        Some(text) => {
            let mut jp = JsonPull::begin_string(text.as_bytes());
            let Some(tree) = jp.read_tree() else {
                return Err(Error::new(
                    EXIT_JSON,
                    format!(
                        "Could not parse filter {}\n{}",
                        text,
                        jp.error.unwrap_or("(null)")
                    ),
                ));
            };
            Some(Filter::new(tree, true)?)
        }
        None => None,
    };

    let mut tiles = Vec::new();
    for (name, z, x, y) in &o.sources {
        let data = std::fs::read(name)
            .map_err(|e| Error::new(EXIT_FAILURE, format!("{}: {}", name, os_message(&e))))?;
        let mut tile = MvtTile::default();
        let mut was_compressed = false;
        match tile.try_decode(&data, &mut was_compressed) {
            Ok(true) => {}
            Ok(false) => {
                return Err(Error::new(
                    EXIT_MVT,
                    format!("Couldn't parse tile {}/{}/{}", z, *x as u32, *y as u32),
                ));
            }
            Err(_) => {
                return Err(Error::new(
                    EXIT_PROTOBUF,
                    format!(
                        "PBF decoding error in tile {}/{}/{}",
                        z, *x as u32, *y as u32
                    ),
                ));
            }
        }
        tiles.push(SourceTile {
            tile,
            z: *z,
            x: *x,
            y: *y,
        });
    }

    let out = overzoom(&tiles, o, filter.as_ref());
    let mut f = std::fs::File::create(&o.output)
        .map_err(|e| Error::new(EXIT_FAILURE, format!("{}: {}", o.output, os_message(&e))))?;
    // as tippecanoe-overzoom, which does not check the write
    let _ = std::io::Write::write_all(&mut f, &out);
    Ok(())
}

/// like a minimal serial_feature, but with mvt_feature-style attributes
struct TileFeature<'a> {
    geom: DrawVec,
    t: i32,
    has_id: bool,
    id: u64,
    tags: Vec<u32>,
    layer: &'a MvtLayer,
    seq: usize,
}

fn should_keep(
    key: &str,
    keep: &BTreeSet<String>,
    exclude: &BTreeSet<String>,
    exclude_prefix: &[String],
) -> bool {
    if keep.is_empty() || keep.contains(key) {
        if exclude.contains(key) {
            return false;
        }

        for prefix in exclude_prefix {
            if starts_with(key, prefix) {
                return false;
            }
        }

        return true;
    }

    false
}

fn handle_closepath_from_mvt(geom: &mut DrawVec) {
    // mvt geometries close polygons with a mvt_closepath operation
    // tippecanoe-internal geometries close polygons with a lineto to the initial point

    let mut last_open = 0;

    for i in 0..geom.len() {
        if geom[i].op as i32 == MVT_CLOSEPATH {
            geom[i] = Draw::new(MVT_LINETO as i8, geom[last_open].x, geom[last_open].y);
        } else if geom[i].op as i32 == MVT_MOVETO {
            last_open = i;
        }
    }
}

// returns true if a feature was output; false if it was clipped away
#[allow(clippy::too_many_arguments)]
fn feature_out(
    features: &[TileFeature],
    outlayer: &mut MvtLayer,
    keep: &BTreeSet<String>,
    exclude: &BTreeSet<String>,
    exclude_prefix: &[String],
    attribute_accum: &HashMap<String, AttributeOp>,
    key_pool: &mut KeyPool,
    buffer: i32,
    include_nonaggregate: bool,
    deduplicate_ids: Option<&mut BTreeSet<u64>>,
) -> bool {
    // Add geometry to output feature

    let mut geom = features[0].geom.clone();
    let t = features[0].t;

    let mut fix_polygons = false;
    if (buffer >= 0) && t == VT_POLYGON {
        fix_polygons = true;
    }

    if fix_polygons {
        handle_closepath_from_mvt(&mut geom);
    }

    if buffer >= 0 {
        if t == VT_LINE {
            geom = clip_lines(&mut geom, 32 - outlayer.detail(), buffer as i64);
        } else if t == VT_POLYGON {
            let mut dv = DrawVec::new();
            geom = simple_clip_poly(&mut geom, 32 - outlayer.detail(), buffer, &mut dv, false);
        } else if t == VT_POINT {
            geom = clip_point(&mut geom, 32 - outlayer.detail(), buffer as i64);
        }

        geom = remove_noop(geom, t, 0);
    }

    if fix_polygons {
        geom = clean_or_clip_poly(&mut geom, 0, 0, false, false);
        geom = close_poly(&mut geom);
    }

    let mut outfeature = MvtFeature {
        type_: features[0].t,
        ..Default::default()
    };
    for g in &geom {
        outfeature
            .geometry
            .push(MvtGeometry::new(g.op as i32, g.x, g.y));
    }

    // ID and attributes, if it didn't get clipped away

    if features[0].has_id
        && let Some(deduplicate_ids) = deduplicate_ids
    {
        if deduplicate_ids.contains(&features[0].id) {
            outfeature.geometry.clear();
        } else {
            deduplicate_ids.insert(features[0].id);
        }
    }

    if !outfeature.geometry.is_empty() {
        if features[0].has_id {
            outfeature.has_id = true;
            outfeature.id = features[0].id;
        }

        outfeature.seq = features[0].seq;

        if !attribute_accum.is_empty() {
            // convert the attributes of the output feature
            // from layer references to a vector so they can have
            // attributes from the other features of the
            // multiplier cluster accumulated onto them

            let mut full_keys: Vec<Arc<String>> = Vec::new();
            let mut full_values: Vec<MvtValue> = Vec::new();

            let f = &features[0];
            let mut i = 0;
            while i + 1 < f.tags.len() {
                let key = &f.layer.keys[f.tags[i] as usize];
                if should_keep(key, keep, exclude, exclude_prefix) {
                    if attribute_accum.contains_key(key) {
                        // this attribute has an accumulator, so convert it
                        full_keys.push(key_pool.pool(key));
                        full_values.push(f.layer.values[f.tags[i + 1] as usize].clone());
                    } else if include_nonaggregate {
                        // otherwise just tag it directly onto the output feature
                        outlayer.tag(
                            &mut outfeature,
                            key,
                            &f.layer.values[f.tags[i + 1] as usize],
                        );
                    }
                }
                i += 2;
            }

            // accumulate whatever attributes are specified to be accumulated
            // onto the feature that will survive into the output, from the
            // features that will not

            for fi in &features[1..] {
                let mut keys: BTreeSet<String> = BTreeSet::new();

                let mut j = 0;
                while j + 1 < fi.tags.len() {
                    let key = &fi.layer.keys[fi.tags[j] as usize];
                    if should_keep(key, keep, exclude, exclude_prefix) {
                        keys.insert(key.clone());
                    }
                    j += 2;
                }

                let mut j = 0;
                while j + 1 < fi.tags.len() {
                    let key = &fi.layer.keys[fi.tags[j] as usize];
                    if should_keep(key, keep, exclude, exclude_prefix)
                        && let Some(found) = attribute_accum.get(key)
                    {
                        let val = fi.layer.values[fi.tags[j + 1] as usize].clone();
                        preserve_attribute_mvt(
                            found,
                            key,
                            &val,
                            &mut full_keys,
                            &mut full_values,
                            key_pool,
                        );
                    }
                    j += 2;
                }
            }

            // convert the final attributes back to mvt_value
            // and tag them onto the output feature

            for i in 0..full_keys.len() {
                if should_keep(&full_keys[i], keep, exclude, exclude_prefix) {
                    outlayer.tag(&mut outfeature, &full_keys[i], &full_values[i]);
                }
            }
        } else if include_nonaggregate {
            let f = &features[0];
            let mut i = 0;
            while i + 1 < f.tags.len() {
                if should_keep(
                    &f.layer.keys[f.tags[i] as usize],
                    keep,
                    exclude,
                    exclude_prefix,
                ) {
                    outlayer.tag(
                        &mut outfeature,
                        &f.layer.keys[f.tags[i] as usize],
                        &f.layer.values[f.tags[i + 1] as usize],
                    );
                }
                i += 2;
            }
        }

        outlayer.features.push(outfeature);
        return true;
    }

    false
}

const RETAIN_POINTS_MULTIPLIER_FIRST: &str = "tippecanoe:retain_points_multiplier_first";
const RETAIN_POINTS_MULTIPLIER_SEQUENCE: &str = "tippecanoe:retain_points_multiplier_sequence";

/// tippecanoe-rs's `overzoom_source_tiles`, with the settings that
/// tippecanoe-overzoom gives it. Returns the encoded tile, or nothing if it
/// has no features.
pub fn overzoom(tiles: &[SourceTile], o: &OverzoomOptions, filter: Option<&Filter>) -> Vec<u8> {
    let OverzoomOptions {
        nz,
        nx,
        ny,
        detail,
        buffer,
        ref keep,
        ref exclude,
        ref exclude_prefix,
        do_compress,
        demultiply,
        preserve_input_order,
        ref attribute_accum,
        ref unidecode_data,
        simplification,
        tiny_polygon_size,
        deduplicate_by_id,
        ..
    } = *o;
    let detail_or_unspecified = detail;
    let mut outtile = MvtTile::default();
    let mut key_pool = KeyPool::default();

    // map from layer name to ids used in that layer
    let mut deduplicate_ids: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();

    for tile in tiles {
        for layer in &tile.tile.layers {
            let mut det = detail_or_unspecified;
            if det <= 0 {
                det = ((layer.extent as f64).ln() / 2f64.ln()).round() as i32;
            }

            // the last layer of the name
            let outlayer = match outtile.layers.iter().rposition(|l| l.name == layer.name) {
                Some(o) => o,
                None => {
                    let newlayer = MvtLayer {
                        name: layer.name.clone(),
                        version: layer.version,
                        extent: 1i64 << det,
                        ..Default::default()
                    };

                    outtile.layers.push(newlayer);
                    outtile.layers.len() - 1
                }
            };

            let mut deduplicate_by_id_set: Option<&mut BTreeSet<u64>> = None;
            if deduplicate_by_id {
                deduplicate_by_id_set =
                    Some(deduplicate_ids.entry(layer.name.clone()).or_default());
            }

            let mut pending_tile_features: Vec<TileFeature> = Vec::new();
            let mut accum_area: f64 = 0.0;

            for feature_in in &layer.features {
                let mut feature = feature_in.clone();
                let mut geom = DrawVec::new();
                let t = feature.type_;

                // Convert feature geometry to world coordinates

                let tilesize: i64 = 1i64 << (32 - tile.z); // source tile size in world coordinates
                let mut ring_closure = Draw::new(0, 0, 0);
                let sametile = nz == tile.z
                    && nx == tile.x
                    && ny == tile.y
                    && outtile.layers[outlayer].extent >= layer.extent;

                for g in &feature.geometry {
                    if g.op == MVT_CLOSEPATH {
                        geom.push(ring_closure);
                    } else {
                        geom.push(Draw::new(
                            g.op as i8,
                            g.x * tilesize / layer.extent + tile.x as i64 * tilesize,
                            g.y * tilesize / layer.extent + tile.y as i64 * tilesize,
                        ));
                        if g.op == MVT_MOVETO {
                            ring_closure = geom[geom.len() - 1];
                            ring_closure.op = MVT_LINETO as i8;
                        }
                    }
                }

                // Now offset from world coordinates to output tile coordinates,
                // but retain world scale, because that is what tippecanoe zoom-oriented clipping expects

                let outtilesize: i64 = 1i64 << (32 - nz); // destination tile size in world coordinates
                for g in geom.iter_mut() {
                    g.x -= nx as i64 * outtilesize;
                    g.y -= ny as i64 * outtilesize;
                }

                {
                    // Clip to output tile

                    let mut xmin = i64::MAX;
                    let mut ymin = i64::MAX;
                    let mut xmax = i64::MIN;
                    let mut ymax = i64::MIN;

                    for g in &geom {
                        xmin = std::cmp::min(xmin, g.x);
                        ymin = std::cmp::min(ymin, g.y);
                        xmax = std::cmp::max(xmax, g.x);
                        ymax = std::cmp::max(ymax, g.y);
                    }

                    let b = outtilesize * buffer as i64 / 256;
                    if xmax < -b || ymax < -b || xmin > outtilesize + b || ymin > outtilesize + b {
                        // quick exclusion by bounding box
                        if deduplicate_by_id && feature.has_id {
                            deduplicate_by_id_set
                                .as_deref_mut()
                                .unwrap()
                                .insert(feature.id);
                        }
                        continue;
                    }

                    if t == VT_LINE {
                        geom = clip_lines(&mut geom, nz, buffer as i64);
                    } else if t == VT_POLYGON {
                        let mut dv = DrawVec::new();
                        geom = simple_clip_poly(&mut geom, nz, buffer, &mut dv, false);
                    } else if t == VT_POINT {
                        geom = clip_point(&mut geom, nz, buffer as i64);
                    }
                }

                if geom.is_empty() {
                    // clipped away
                    if deduplicate_by_id && feature.has_id {
                        deduplicate_by_id_set
                            .as_deref_mut()
                            .unwrap()
                            .insert(feature.id);
                    }
                    continue;
                }

                let mut flush_multiplier_cluster = false;
                if demultiply {
                    let mut i: isize = feature.tags.len() as isize - 2;
                    while i >= 0 {
                        let iu = i as usize;
                        if layer.keys[feature.tags[iu] as usize] == RETAIN_POINTS_MULTIPLIER_FIRST {
                            let v = &layer.values[feature.tags[iu + 1] as usize];
                            if v.type_ == MVT_BOOL && v.numeric_value.bool_value() {
                                flush_multiplier_cluster = true;
                                feature.tags.drain(iu..iu + 2);
                            }
                        } else if i < feature.tags.len() as isize
                            && layer.keys[feature.tags[iu] as usize]
                                == RETAIN_POINTS_MULTIPLIER_SEQUENCE
                        {
                            let v = &layer.values[feature.tags[iu + 1] as usize];
                            feature.seq = mvt_value_to_long_long(v) as usize;
                            feature.tags.drain(iu..iu + 2);
                        }
                        i -= 2;
                    }
                } else {
                    flush_multiplier_cluster = true;
                }

                if flush_multiplier_cluster && !pending_tile_features.is_empty() {
                    feature_out(
                        &pending_tile_features,
                        &mut outtile.layers[outlayer],
                        keep,
                        exclude,
                        exclude_prefix,
                        attribute_accum,
                        &mut key_pool,
                        -1,
                        true,
                        deduplicate_by_id_set.as_deref_mut(),
                    );
                    pending_tile_features.clear();
                }

                // As in tippecanoe, the attributes an attribute-filter would
                // exclude are not excluded.
                let mut exclude_attributes: BTreeSet<String> = BTreeSet::new();
                // the geometry, clipped to the output tile, back in world coordinates
                let filter_geom = FilterGeometry {
                    draws: &geom,
                    origin: (nx as i64 * outtilesize, ny as i64 * outtilesize),
                    type_: t,
                };
                if let Some(filter) = filter
                    && !filter.evaluate_feature(
                        &feature,
                        layer,
                        &mut exclude_attributes,
                        nz,
                        unidecode_data,
                        Some(&filter_geom),
                    )
                {
                    // filtered away
                    if deduplicate_by_id && feature.has_id {
                        deduplicate_by_id_set
                            .as_deref_mut()
                            .unwrap()
                            .insert(feature.id);
                    }
                    continue;
                }

                let mut still_need_simplification_after_reduction = false;
                if t == VT_POLYGON && tiny_polygon_size > 0.0 {
                    let mut simplified_away_by_reduction = false;

                    geom = reduce_tiny_poly(
                        &geom,
                        nz,
                        det,
                        &mut still_need_simplification_after_reduction,
                        &mut simplified_away_by_reduction,
                        &mut accum_area,
                        tiny_polygon_size,
                    );
                } else {
                    still_need_simplification_after_reduction = true;
                }

                if simplification > 0.0 && still_need_simplification_after_reduction {
                    if t == VT_POLYGON {
                        geom = simplify_lines_basic(&mut geom, nz, det, simplification, 4);
                    } else if t == VT_LINE {
                        geom = simplify_lines_basic(&mut geom, nz, det, simplification, 0);
                    }
                }

                // Scale to output tile extent

                to_tile_scale(&mut geom, nz, det);

                if !sametile {
                    // Clean geometries

                    geom = remove_noop(geom, t, 0);
                    if t == VT_POLYGON {
                        geom = clean_or_clip_poly(&mut geom, 0, 0, false, false);
                    }
                }

                if t == VT_POLYGON {
                    geom = close_poly(&mut geom);
                }

                if geom.is_empty() {
                    // simplified away
                    if deduplicate_by_id && feature.has_id {
                        deduplicate_by_id_set
                            .as_deref_mut()
                            .unwrap()
                            .insert(feature.id);
                    }
                    continue;
                }

                let tf = TileFeature {
                    geom,
                    t,
                    has_id: feature.has_id,
                    id: feature.id,
                    tags: std::mem::take(&mut feature.tags),
                    layer,
                    seq: feature.seq,
                };

                pending_tile_features.push(tf);
            }

            if !pending_tile_features.is_empty() {
                feature_out(
                    &pending_tile_features,
                    &mut outtile.layers[outlayer],
                    keep,
                    exclude,
                    exclude_prefix,
                    attribute_accum,
                    &mut key_pool,
                    -1,
                    true,
                    deduplicate_by_id_set,
                );
                pending_tile_features.clear();
            }

            if preserve_input_order {
                outtile.layers[outlayer].features.sort_by_key(|f| f.seq);
            }
        }
    }

    outtile.layers.retain(|layer| !layer.features.is_empty());

    if !outtile.layers.is_empty() {
        let pbf = outtile.encode();

        let mut compressed = Vec::new();
        if do_compress {
            compress(&pbf, &mut compressed, true);
        } else {
            compressed = pbf;
        }

        compressed
    } else {
        Vec::new()
    }
}

// cut-down version of simplify_lines(), not dealing with shared node preservation
fn simplify_lines_basic(
    geom: &mut DrawVec,
    z: i32,
    detail: i32,
    simplification: f64,
    retain: usize,
) -> DrawVec {
    let res: i32 = 1i32.wrapping_shl((32 - detail - z) as u32);

    for g in geom.iter_mut() {
        if g.op == VT_MOVETO {
            g.necessary = 1;
        } else if g.op == VT_LINETO {
            g.necessary = 0;
            // if this is actually the endpoint, not an intermediate point,
            // it will be marked as necessary below
        } else {
            g.necessary = 1;
        }
    }

    let mut i = 0;
    while i < geom.len() {
        if geom[i].op == VT_MOVETO {
            let mut j = i + 1;
            while j < geom.len() {
                if geom[j].op != VT_LINETO {
                    break;
                }
                j += 1;
            }

            geom[i].necessary = 1;
            geom[j - 1].necessary = 1;

            if j - i > 1 {
                douglas_peucker(
                    geom,
                    i as i32,
                    (j - i) as i32,
                    res as f64 * simplification,
                    2,
                    retain,
                    false,
                );
            }
            i = j - 1;
        }
        i += 1;
    }

    let mut out = 0;
    for i in 0..geom.len() {
        if geom[i].necessary != 0 {
            geom[out] = geom[i];
            out += 1;
        }
    }
    geom.truncate(out);
    geom.clone()
}
