//! Reading input files into a `FeatureStore`, in parallel. See DESIGN.md.
//!
//! tippecanoe reads an input in one of three ways, and they number the
//! features (tippecanoe's `seq`, which `--generate-ids` makes into ids)
//! differently:
//!
//! - With `-P`, or if the input starts with a record separator, a file it can
//!   map is split into `CPUS` segments at separators, each parsed on its own
//!   and numbered from its byte offset.
//! - The same from a pipe or a gzipped file goes in chunks of more than 10 MB,
//!   each split into segments the same way.
//! - Otherwise the input is parsed as one stream and numbered from 0.
//!
//! Each of those parses, a "unit" here, is split further into byte ranges at
//! line or record boundaries, which are parsed in parallel as if each started
//! a parse of its own. A range that ends inside a JSON value, or that takes a
//! bare geometry the unit's parse would have refused, is read again as part
//! of the unit, so the result is always what the unit's own parse gives.

pub mod chunk;
pub mod csv;
pub mod flatgeobuf;
pub mod geobuf;
pub mod geojson;
pub mod json;
pub mod serialize;
pub mod serialized;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Write};

use rayon::prelude::*;
use tippecanoe::errors::*;

use chunk::{Chunk, Interner, Message, MessageKind, SHARDS, Stat};
use serialize::Config;

use crate::Error;
use crate::buf::Buf;
use crate::options::{Options, Source, os_message};
use crate::store::{Attr, Chunked, Feature, FeatureStore, Geometry, Layer, Values};

/// The bytes of one input, mapped or read into memory.
enum Data {
    Map(memmap2::Mmap),
    Owned(Vec<u8>),
}

impl std::ops::Deref for Data {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Data::Map(m) => m,
            Data::Owned(v) => v,
        }
    }
}

/// One parse that tippecanoe makes of part of an input.
#[derive(Clone, Copy, Debug)]
struct Unit {
    start: usize,
    end: usize,
    /// The `seq` of the unit's first feature.
    seq_base: i64,
    /// Whether the unit is one of tippecanoe's parallel segments, whose
    /// `-zg` sums are added up separately, rather than its serial stream.
    segment: bool,
    /// The reader, tippecanoe's segment number, that reads the unit.
    reader: u16,
}

/// A chunk that the unit's parse confirmed, with where it sits.
struct Accepted {
    chunk: Chunk,
    unit: usize,
    /// Lines of the unit before the chunk, for numbering its messages.
    line_offset: i32,
}

/// The name tippecanoe gives a layer from its file name.
fn layer_name_from_file(src: &str, l: usize) -> String {
    let src = src.split('\0').next().unwrap_or("").as_bytes();
    // Find the last component of the pathname
    let mut use_ = 0;
    for ocp in 0..src.len() {
        if src[ocp] == b'/' && ocp + 1 < src.len() {
            use_ = ocp + 1;
        }
    }
    let mut trunc: Vec<u8> = src[use_..].to_vec();

    let trim: [&[u8]; 7] = [
        b".json",
        b".geojson",
        b".geobuf",
        b".mbtiles",
        b".pmtiles",
        b".csv",
        b".gz",
    ];

    // Trim .json or .mbtiles from the name
    let mut again = true;
    while again {
        again = false;
        for t in trim.iter() {
            if trunc.len() > t.len() && trunc.ends_with(t) {
                trunc.truncate(trunc.len() - t.len());
                again = true;
            }
        }
    }

    // Trim out characters that can't be part of selector
    let out: Vec<u8> = trunc
        .into_iter()
        .filter(|&c| c.is_ascii_alphabetic() || c.is_ascii_digit() || c == b'_' || (c & 0x80) != 0)
        .collect();

    let name = String::from_utf8_lossy(&out).into_owned();
    if name.is_empty() || !tippecanoe::text::check_utf8(&out).is_empty() {
        format!("unknown{}", l)
    } else {
        name
    }
}

/// The layer name tippecanoe gives source `l` when none is given: from its
/// file name, or for standard input from the tileset name.
pub fn layer_name_for(file: &str, tileset_name: &str, l: usize) -> String {
    if file.is_empty() {
        layer_name_from_file(tileset_name, l)
    } else {
        layer_name_from_file(file, l)
    }
}

/// Prints the messages of a chunk with the lines numbered within its unit.
fn print_messages(
    out: &mut Vec<u8>,
    fname: &str,
    messages: &[Message],
    line_offset: i32,
    once: &mut HashSet<&'static str>,
) {
    for m in messages {
        if let MessageKind::Once(k) = m.kind
            && !once.insert(k)
        {
            continue;
        }
        write_parts(out, fname, &m.parts, line_offset);
    }
}

fn write_parts(out: &mut Vec<u8>, fname: &str, parts: &[(Option<i32>, Vec<u8>)], off: i32) {
    for (line, text) in parts {
        if let Some(line) = line {
            out.extend_from_slice(format!("{}:{}: ", fname, line + off).as_bytes());
        }
        out.extend_from_slice(text);
        out.push(b'\n');
    }
}

fn count_lines(data: &[u8]) -> i32 {
    data.iter().filter(|&&c| c == b'\n').count() as i32
}

/// Splits a unit into ranges of about `target` bytes, each starting at a separator.
fn split_unit(data: &[u8], unit: &Unit, sep: u8, target: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = unit.start;
    while start < unit.end {
        let mut end = (start + target).min(unit.end);
        while end < unit.end && data[end] != sep {
            end += 1;
        }
        out.push((start, end));
        start = end;
    }
    if out.is_empty() {
        out.push((unit.start, unit.end));
    }
    out
}

/// tippecanoe's segments of `data[start..end]` for a parallel read.
fn segments(data: &[u8], start: usize, end: usize, cpus: usize, sep: u8) -> Vec<usize> {
    let len = end - start;
    let mut segs = vec![start; cpus + 1];
    segs[cpus] = end;
    for (i, s) in segs.iter_mut().enumerate().take(cpus).skip(1) {
        let mut p = len * i / cpus;
        while p < len && data[start + p] != sep {
            p += 1;
        }
        *s = start + p;
    }
    segs
}

/// The chunks of tippecanoe's streaming parallel read: each ends at the
/// first 2000-byte read boundary past 10 MB that ends in a separator.
fn stream_chunks(data: &[u8], sep: u8) -> Vec<(usize, usize)> {
    const READ_BUF: usize = 2000;
    const PARSE_MIN: usize = 10000000;
    let mut out = Vec::new();
    let mut start = 0;
    let mut p = READ_BUF;
    while p < data.len() {
        if p - start > PARSE_MIN && data[p - 1] == sep {
            out.push((start, p));
            start = p;
        }
        p += READ_BUF;
    }
    if start < data.len() {
        out.push((start, data.len()));
    }
    out
}

fn parse_range(
    data: &[u8],
    range: (usize, usize),
    reading: &geojson::Reading<'_>,
    layers: &[String],
    found_features: i64,
) -> Chunk {
    let mut chunk = Chunk::new(layers);
    let input = &data[range.0..range.1];
    geojson::parse(input, reading, &mut chunk, found_features);
    chunk
}

/// Parses the units of one input in parallel and confirms the chunks in the
/// order of each unit's own parse.
#[allow(clippy::too_many_arguments)]
fn read_units(
    data: &[u8],
    mapped: bool,
    units: &[Unit],
    sep: u8,
    reading: &geojson::Reading<'_>,
    layers: &[String],
    threads: usize,
) -> Vec<Accepted> {
    let total: usize = units.iter().map(|u| u.end - u.start).sum();
    let target = match reading.cfg.o.chunk_bytes {
        0 => (total / (threads * 8).max(1)).clamp(1 << 18, 1 << 24),
        n => n,
    };

    let ranges: Vec<(usize, (usize, usize))> = units
        .iter()
        .enumerate()
        .flat_map(|(u, unit)| {
            split_unit(data, unit, sep, target)
                .into_iter()
                .map(move |r| (u, r))
        })
        .collect();

    // the lines of each range are counted while its pages are at hand
    let chunks: Vec<(Chunk, i32)> = ranges
        .par_iter()
        .map(|&(_, r)| {
            let chunk = parse_range(data, r, reading, layers, 0);
            let lines = count_lines(&data[r.0..r.1]);
            if mapped {
                crate::mem::release_mapped(&data[r.0..r.1]);
            }
            (chunk, lines)
        })
        .collect();

    let mut accepted = Vec::new();
    let mut chunks = chunks.into_iter();
    let mut i = 0;
    while i < ranges.len() {
        let u = ranges[i].0;
        let unit = &units[u];
        let mut found_features: i64 = 0;
        let mut line_offset: i32 = 0;
        let mut stopped = false;

        while i < ranges.len() && ranges[i].0 == u {
            let range = ranges[i].1;
            let (mut chunk, lines) = chunks.next().unwrap();
            i += 1;
            if stopped {
                continue;
            }

            let last = i == ranges.len() || ranges[i].0 != u;
            if chunk.bare_before_feature && found_features > 0 {
                chunk = parse_range(data, range, reading, layers, found_features);
            }
            let unclean = chunk.error.is_some() && (chunk.cut_short || chunk.error_at_end);
            if !last && unclean {
                // The unit's parse reads on into the next range, so read the
                // rest of the unit as one range.
                let rest = (range.0, unit.end);
                let chunk = parse_range(data, rest, reading, layers, found_features);
                accepted.push(Accepted {
                    chunk,
                    unit: u,
                    line_offset,
                });
                stopped = true;
                continue;
            }

            found_features += chunk.found_features;
            let ends = chunk.error.is_some() || chunk.fatal.is_some();
            accepted.push(Accepted {
                chunk,
                unit: u,
                line_offset,
            });
            line_offset += lines;
            if ends {
                // tippecanoe stops reading this unit at its error.
                stopped = true;
            }
        }
    }

    accepted
}

/// Everything read so far, before it is merged into the store.
struct Parts {
    chunks: Vec<Accepted>,
    /// Per accepted chunk: the `seq` of its first feature.
    seq_base: Vec<i64>,
    /// Per accepted chunk: the segment that read it.
    segment: Vec<u16>,
    dist_sum: f64,
    dist_count: usize,
    area_sum: f64,
}

/// Adds the accepted chunks of one input to `parts`, numbering their features
/// as tippecanoe does and adding up the `-zg` sums in tippecanoe's order.
fn add_chunks(parts: &mut Parts, units: &[Unit], chunks: Vec<Accepted>, unit_offset: usize) {
    let mut rank_in_unit: Vec<i64> = vec![0; units.len()];
    let mut unit_sums: Vec<(f64, f64)> = vec![(0.0, 0.0); units.len()];
    let mut segment_units: Vec<usize> = Vec::new();

    for a in chunks {
        let u = a.unit - unit_offset;
        let unit = &units[u];
        parts.seq_base.push(unit.seq_base + rank_in_unit[u]);
        parts.segment.push(unit.reader);
        rank_in_unit[u] += a.chunk.features.len() as i64;

        parts.dist_count += a.chunk.dist_count;
        if unit.segment {
            let s = &mut unit_sums[u];
            for t in &a.chunk.dist_terms {
                s.0 += t;
            }
            for t in &a.chunk.area_terms {
                s.1 += t;
            }
            if segment_units.last() != Some(&u) {
                segment_units.push(u);
            }
        } else {
            for t in &a.chunk.dist_terms {
                parts.dist_sum += t;
            }
            for t in &a.chunk.area_terms {
                parts.area_sum += t;
            }
        }
        parts.chunks.push(Accepted { unit: a.unit, ..a });
    }

    // tippecanoe adds each segment's sums once the segments are joined, in
    // segment order, including the segments that found nothing.
    for (u, unit) in units.iter().enumerate() {
        if unit.segment {
            parts.dist_sum += unit_sums[u].0;
            parts.area_sum += unit_sums[u].1;
        }
    }
}

/// Reads all of standard input, or a file that cannot be mapped.
fn read_all(r: &mut dyn Read, fname: &str, gz: bool) -> crate::Result<Vec<u8>> {
    let mut raw = Vec::new();
    r.read_to_end(&mut raw)
        .map_err(|e| Error::new(EXIT_READ, format!("{}: {}", fname, os_message(&e))))?;
    if gz && raw.len() >= 2 && raw[0] == 0x1F && raw[1] == 0x8B {
        let mut out = Vec::new();
        let mut d = flate2::read::MultiGzDecoder::new(&raw[..]);
        d.read_to_end(&mut out).map_err(|_| {
            Error::new(
                EXIT_READ,
                format!("{}: Error reading compressed data", fname),
            )
        })?;
        return Ok(out);
    }
    // zlib reads data that is not gzipped as it is
    Ok(raw)
}

/// Opens a source: the mapped file, or None if tippecanoe would read it as a stream.
fn open_source(src: &Source) -> Result<(Option<std::fs::File>, Option<memmap2::Mmap>), Error> {
    if src.file.is_empty() {
        return Ok((None, None));
    }
    let f = std::fs::File::open(&src.file)
        .map_err(|e| Error::new(EXIT_OPEN, format!("{}: {}", src.file, os_message(&e))))?;
    let is_gz = src.file.len() > 3 && src.file.ends_with(".gz");
    let mut map = None;
    if !is_gz
        && let Ok(md) = f.metadata()
        && md.len() > 0
    {
        // SAFETY: the input is not changed while it is being read.
        map = unsafe { memmap2::Mmap::map(&f) }.ok();
    }
    Ok((Some(f), map))
}

/// Reads every input named in `options` into one store, in input order.
pub fn read_inputs(options: &Options) -> crate::Result<FeatureStore> {
    let threads = options.thread_count();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|e| Error::new(EXIT_PTHREAD, e.to_string()))?;
    pool.install(|| read_inputs1(options, threads))
}

fn read_inputs1(options: &Options, threads: usize) -> crate::Result<FeatureStore> {
    let cfg = Config::new(options);
    let mut sources = options.all_sources();
    let fname = options.tileset_name();
    let cpus = options.tippecanoe_cpus.max(1);
    let mut stderr: Vec<u8> = Vec::new();

    for (l, s) in sources.iter_mut().enumerate() {
        if s.layer.is_empty() {
            s.layer = layer_name_for(&s.file, &fname, l);
            if !options.quiet {
                stderr.extend_from_slice(
                    format!("For layer {}, using name \"{}\"\n", l, s.layer).as_bytes(),
                );
            }
        }
    }

    // The layers every reader starts with: one per source layer name, with
    // the description of the first source that names it.
    let mut layer_names: Vec<String> = Vec::new();
    let mut layer_source: BTreeMap<String, usize> = BTreeMap::new();
    for (l, s) in sources.iter().enumerate() {
        if !layer_source.contains_key(&s.layer) {
            layer_source.insert(s.layer.clone(), l);
            layer_names.push(s.layer.clone());
        }
    }

    let mut parts = Parts {
        chunks: Vec::new(),
        seq_base: Vec::new(),
        segment: Vec::new(),
        dist_sum: 0.0,
        dist_count: 0,
        area_sum: 0.0,
    };
    let mut once: HashSet<&'static str> = HashSet::new();
    let mut overall_offset: i64 = 0;
    let mut unit_count = 0;

    for source in 0..sources.len() {
        let src = &sources[source];
        let reading_name = if src.file.is_empty() {
            "standard input".to_string()
        } else {
            src.file.clone()
        };
        let layer = layer_source[&src.layer];

        let (file, map) = match open_source(src) {
            Ok(fm) => fm,
            Err(e) => {
                // tippecanoe reports it and goes on to the next input
                stderr.extend_from_slice(e.message.as_bytes());
                stderr.push(b'\n');
                continue;
            }
        };

        let format = src.format.as_str();
        let name = src.file.as_str();
        if format == "fgb" || (name.len() > 4 && name.ends_with(".fgb")) {
            let Some(map) = map else {
                flush(&mut stderr);
                return Err(Error::new(
                    EXIT_MEMORY,
                    format!(
                        "{}: mmap: {}: {}",
                        options.program,
                        reading_name,
                        tippecanoe::cstd::strerror(libc::EINVAL)
                    ),
                ));
            };
            let layername = sources[layer].layer.clone();
            read_fgb(
                &mut parts,
                &map,
                &cfg,
                &reading_name,
                &layername,
                &layer_names,
                cpus,
                &mut overall_offset,
                &mut once,
                &mut stderr,
            )?;
            continue;
        }
        if format == "geobuf" || (name.len() > 7 && name.ends_with(".geobuf")) {
            let Some(map) = map else {
                flush(&mut stderr);
                return Err(Error::new(
                    EXIT_MEMORY,
                    format!(
                        "{}: mmap: {}: {}",
                        options.program,
                        reading_name,
                        tippecanoe::cstd::strerror(libc::EINVAL)
                    ),
                ));
            };
            let layername = sources[layer].layer.clone();
            read_geobuf(
                &mut parts,
                &map,
                &cfg,
                &reading_name,
                &layername,
                &layer_names,
                cpus,
                &mut overall_offset,
                &mut once,
                &mut stderr,
            )?;
            continue;
        }
        if format == "csv" || (name.len() > 4 && name.ends_with(".csv")) {
            drop(map);
            let mut chunk = Chunk::new(&layer_names);
            let r = match file {
                Some(f) => csv::parse(
                    &mut std::io::BufReader::new(f),
                    &src.file,
                    &cfg,
                    &sources[layer].layer,
                    &mut chunk,
                ),
                None => csv::parse(
                    &mut std::io::stdin().lock(),
                    &src.file,
                    &cfg,
                    &sources[layer].layer,
                    &mut chunk,
                ),
            };
            print_messages(&mut stderr, &reading_name, &chunk.messages, 0, &mut once);
            flush(&mut stderr);
            r?;
            // tippecanoe does not add up the -zg sums of CSV input
            chunk.dist_terms.clear();
            chunk.dist_count = 0;
            chunk.area_terms.clear();
            let n = chunk.features.len() as i64;
            parts.seq_base.push(overall_offset);
            parts.segment.push(0);
            parts.chunks.push(Accepted {
                chunk,
                unit: usize::MAX,
                line_offset: 0,
            });
            overall_offset += n;
            continue;
        }

        let mut read_parallel_this: u8 = if options.read_parallel { b'\n' } else { 0 };
        let mut data: Option<Data> = None;
        if let Some(m) = map {
            if m[0] == 0x1E {
                read_parallel_this = 0x1E;
            }
            if read_parallel_this != 0 {
                data = Some(Data::Map(m));
            }
        }

        let reading = geojson::Reading {
            cfg: &cfg,
            fname: &reading_name,
            layer: layer as i32,
            layername: &sources[layer].layer,
        };

        let (units, sep, data) = if let Some(data) = data {
            // A mapped file read in parallel segments.
            let len = data.len();
            let segs = segments(&data, 0, len, cpus, read_parallel_this);
            let units: Vec<Unit> = (0..cpus)
                .map(|i| Unit {
                    start: segs[i],
                    end: segs[i + 1],
                    seq_base: segs[i] as i64 + overall_offset,
                    segment: true,
                    reader: i as u16,
                })
                .collect();
            overall_offset += len as i64;
            (units, read_parallel_this, data)
        } else {
            let gz = {
                let n = &sources[layer].file;
                n.len() > 3 && n.ends_with(".gz")
            };
            let bytes = match file {
                Some(mut f) => read_all(&mut f, &reading_name, gz)?,
                None => read_all(&mut std::io::stdin().lock(), &reading_name, gz)?,
            };
            if bytes.first() == Some(&0x1E) {
                read_parallel_this = 0x1E;
            }
            if read_parallel_this != 0 {
                // Read in chunks, each parsed in parallel segments.
                let mut units = Vec::new();
                let mut initial_offset = overall_offset;
                for (cs, ce) in stream_chunks(&bytes, read_parallel_this) {
                    let segs = segments(&bytes, cs, ce, cpus, read_parallel_this);
                    for i in 0..cpus {
                        units.push(Unit {
                            start: segs[i],
                            end: segs[i + 1],
                            seq_base: (segs[i] - cs) as i64 + initial_offset,
                            segment: true,
                            reader: i as u16,
                        });
                    }
                    initial_offset += (ce - cs) as i64;
                }
                overall_offset += bytes.len() as i64;
                (units, read_parallel_this, Data::Owned(bytes))
            } else {
                let units = vec![Unit {
                    start: 0,
                    end: bytes.len(),
                    seq_base: overall_offset,
                    segment: false,
                    reader: 0,
                }];
                (units, b'\n', Data::Owned(bytes))
            }
        };

        let mapped = matches!(data, Data::Map(_));
        let accepted = read_units(&data, mapped, &units, sep, &reading, &layer_names, threads);

        // Messages in the order of the input, and the first fatal error.
        let mut fatal = None;
        for a in &accepted {
            print_messages(
                &mut stderr,
                &reading_name,
                &a.chunk.messages,
                a.line_offset,
                &mut once,
            );
            if let Some((code, p)) = &a.chunk.fatal {
                let mut m = Vec::new();
                write_parts(&mut m, &reading_name, p, a.line_offset);
                if m.last() == Some(&b'\n') {
                    m.pop();
                }
                fatal = Some(Error::new(*code, String::from_utf8_lossy(&m)));
                break;
            }
            if let Some(e) = &a.chunk.error {
                write_parts(&mut stderr, &reading_name, &e.parts, a.line_offset);
            }
        }
        flush(&mut stderr);
        if let Some(e) = fatal {
            return Err(e);
        }

        let serial_end = if units.len() == 1 && !units[0].segment {
            Some(units[0].seq_base)
        } else {
            None
        };
        let base = unit_count;
        let accepted: Vec<Accepted> = accepted
            .into_iter()
            .map(|a| Accepted {
                unit: a.unit + base,
                ..a
            })
            .collect();
        let read_now: i64 = accepted.iter().map(|a| a.chunk.features.len() as i64).sum();
        add_chunks(&mut parts, &units, accepted, base);
        unit_count += units.len();

        if let Some(start) = serial_end {
            overall_offset = start + read_now;
        }
    }
    flush(&mut stderr);

    Ok(merge(parts, &sources, &layer_names, &layer_source, options))
}

/// Reads a FlatGeobuf file, numbering its features in the batches and ranges
/// that tippecanoe parses them in, and adding up the `-zg` sums as it does.
#[allow(clippy::too_many_arguments)]
fn read_fgb(
    parts: &mut Parts,
    data: &[u8],
    cfg: &Config<'_>,
    reading_name: &str,
    layername: &str,
    layer_names: &[String],
    cpus: usize,
    overall_offset: &mut i64,
    once: &mut HashSet<&'static str>,
    stderr: &mut Vec<u8>,
) -> crate::Result<()> {
    let h = flatgeobuf::read_header(data, cfg.o.quiet)?;

    // The batches: tippecanoe runs its queue when it holds more than CPUS * 500.
    let batch = cpus * 500 + 1;
    let mut ranges: Vec<(usize, usize, usize)> = Vec::new();
    let mut start = 0;
    while start < h.features.len() {
        let end = (start + batch).min(h.features.len());
        let size = end - start;
        for i in 0..cpus {
            ranges.push((start + size * i / cpus, start + size * (i + 1) / cpus, i));
        }
        start = end;
    }

    let chunks: Vec<(Chunk, crate::Result<()>)> = ranges
        .par_iter()
        .map(|&(a, b, _)| {
            let mut chunk = Chunk::new(layer_names);
            let mut r = Ok(());
            for f in a..b {
                let id = if h.sequence_ids { f as i64 } else { -1 };
                r = flatgeobuf::read_feature(&mut chunk, cfg, &h, h.features[f], id, layername);
                if r.is_err() {
                    break;
                }
            }
            (chunk, r)
        })
        .collect();

    let mut seg_dist = vec![0.0f64; cpus];
    let mut seg_area = vec![0.0f64; cpus];
    let mut batch_base = *overall_offset;
    let mut k = 0;
    while k < ranges.len() {
        let size_start = ranges[k].0;
        let mut last_end = batch_base;
        for (j, (chunk, _)) in chunks[k..k + cpus].iter().enumerate() {
            let (a, _, i) = ranges[k + j];
            let base = batch_base + (a - size_start) as i64;
            parts.seq_base.push(base);
            parts.segment.push(i as u16);
            last_end = base + chunk.features.len() as i64;
            for t in &chunk.dist_terms {
                seg_dist[i] += t;
            }
            for t in &chunk.area_terms {
                seg_area[i] += t;
            }
            parts.dist_count += chunk.dist_count;
        }
        batch_base = last_end;
        k += cpus;
    }
    *overall_offset = batch_base;
    for i in 0..cpus {
        parts.dist_sum += seg_dist[i];
        // sic: tippecanoe assigns rather than adds the area sums of FlatGeobuf input
        parts.area_sum = seg_area[i];
    }

    for (chunk, r) in chunks {
        print_messages(stderr, reading_name, &chunk.messages, 0, once);
        if let Err(e) = r {
            flush(stderr);
            return Err(e);
        }
        parts.chunks.push(Accepted {
            chunk,
            unit: usize::MAX,
            line_offset: 0,
        });
    }
    flush(stderr);
    Ok(())
}

/// Reads a Geobuf file, numbering its features in the batches and ranges that
/// tippecanoe parses them in, and adding up the `-zg` sums as it does.
#[allow(clippy::too_many_arguments)]
fn read_geobuf(
    parts: &mut Parts,
    data: &[u8],
    cfg: &Config<'_>,
    reading_name: &str,
    layername: &str,
    layer_names: &[String],
    cpus: usize,
    overall_offset: &mut i64,
    once: &mut HashSet<&'static str>,
    stderr: &mut Vec<u8>,
) -> crate::Result<()> {
    let (events, keys) = geobuf::events(data, cpus)?;

    // The chunks: each bare geometry, and each range of each batch.
    let mut jobs: Vec<(usize, usize, usize)> = Vec::new();
    for (ev, event) in events.iter().enumerate() {
        match event {
            geobuf::Event::Bare(_) => jobs.push((ev, 0, 0)),
            geobuf::Event::Batch(q, _) => {
                let size = q.len();
                for i in 0..cpus {
                    jobs.push((ev, size * i / cpus, size * (i + 1) / cpus));
                }
            }
        }
    }

    let chunks: Vec<(Chunk, crate::Result<()>)> = jobs
        .par_iter()
        .map(|&(ev, a, b)| {
            let mut chunk = Chunk::new(layer_names);
            let r = match &events[ev] {
                geobuf::Event::Bare(g) => {
                    let mut p = g.p;
                    geobuf::read_bare(&mut chunk, cfg, &mut p, g.dim, g.e, layername)
                }
                geobuf::Event::Batch(q, nkeys) => {
                    let mut r = Ok(());
                    for f in &q[a..b] {
                        let mut p = f.p;
                        r = geobuf::read_feature(
                            &mut chunk,
                            cfg,
                            reading_name,
                            &mut p,
                            f.dim,
                            f.e,
                            &keys[..*nkeys],
                            layername,
                        );
                        if r.is_err() {
                            break;
                        }
                    }
                    r
                }
            };
            (chunk, r)
        })
        .collect();

    let mut seg_dist = vec![0.0f64; cpus];
    let mut seg_area = vec![0.0f64; cpus];
    let mut l0 = *overall_offset;
    let mut k = 0;
    while k < jobs.len() {
        let ev = jobs[k].0;
        let n = if let geobuf::Event::Batch(..) = events[ev] {
            cpus
        } else {
            1
        };
        let mut last_end = l0;
        for (i, (chunk, _)) in chunks[k..k + n].iter().enumerate() {
            let base = l0 + jobs[k + i].1 as i64;
            parts.seq_base.push(base);
            parts.segment.push(i as u16);
            last_end = base + chunk.features.len() as i64;
            for t in &chunk.dist_terms {
                seg_dist[i] += t;
            }
            for t in &chunk.area_terms {
                seg_area[i] += t;
            }
            parts.dist_count += chunk.dist_count;
        }
        l0 = last_end;
        k += n;
    }
    *overall_offset = l0;
    for i in 0..cpus {
        parts.dist_sum += seg_dist[i];
        parts.area_sum += seg_area[i];
    }

    for (chunk, r) in chunks {
        print_messages(stderr, reading_name, &chunk.messages, 0, once);
        if let Err(e) = r {
            flush(stderr);
            return Err(e);
        }
        parts.chunks.push(Accepted {
            chunk,
            unit: usize::MAX,
            line_offset: 0,
        });
    }
    flush(stderr);
    Ok(())
}

fn flush(stderr: &mut Vec<u8>) {
    if !stderr.is_empty() {
        let _ = std::io::stderr().write_all(stderr);
        stderr.clear();
    }
}

/// The merged table, and for each chunk its ids in it.
type Merged = (Values, Vec<Buf<u32>>);

/// Interns the entries of every chunk's table in one table, in parallel by
/// shard. Returns the table and, for each chunk, its ids in the table.
fn merge_tables(mut tables: Vec<Interner>) -> Merged {
    let grouped: Vec<(Buf<u32>, [usize; SHARDS + 1])> =
        tables.par_iter().map(|t| t.by_shard()).collect();
    tables.par_iter_mut().for_each(|t| t.stop_interning());
    // each shard's table, its ids for the chunks' entries in chunk order,
    // and where each chunk's start in them
    let shards: Vec<(Interner, Buf<u32>, Vec<usize>)> = (0..SHARDS)
        .into_par_iter()
        .map(|s| {
            let mut table = Interner::default();
            let total = grouped.iter().map(|(_, st)| st[s + 1] - st[s]).sum();
            let mut ids = Buf::with_capacity(total);
            let mut starts = Vec::with_capacity(tables.len() + 1);
            for (t, (list, st)) in tables.iter().zip(&grouped) {
                starts.push(ids.len());
                for &local in &list[st[s]..st[s + 1]] {
                    let (ty, b) = t.get(local);
                    ids.push(table.intern(ty, b));
                }
            }
            starts.push(ids.len());
            (table, ids, starts)
        })
        .collect();

    let mut base = Vec::with_capacity(SHARDS);
    let mut n = 0u32;
    for (t, _, _) in &shards {
        base.push(n);
        n += t.len() as u32;
    }

    let remap: Vec<Buf<u32>> = tables
        .par_iter()
        .enumerate()
        .map(|(c, t)| {
            let mut m = Buf::new();
            m.resize(t.len(), 0u32);
            let (list, st) = &grouped[c];
            for (s, (_, ids, starts)) in shards.iter().enumerate() {
                let local = &list[st[s]..st[s + 1]];
                for (&l, &id) in local.iter().zip(&ids[starts[c]..starts[c + 1]]) {
                    m[l as usize] = base[s] + id;
                }
            }
            m
        })
        .collect();

    drop(tables);
    let mut values = Values::with_capacity(
        n as usize,
        shards.iter().map(|(t, _, _)| t.bytes.len()).sum(),
    );
    for (t, _, _) in shards {
        values.append(&t.types, &t.ends, &t.bytes);
    }

    (values, remap)
}

/// The chunks' columns that are empty while all their items are 0, joined
/// in the same way.
fn join_sparse<T: Copy + Default>(
    parts: &mut Parts,
    counts: &[usize],
    column: impl Fn(&mut Chunk) -> &mut Buf<T>,
) -> Buf<T> {
    let mut out = Buf::new();
    if parts
        .chunks
        .iter_mut()
        .all(|a| column(&mut a.chunk).is_empty())
    {
        return out;
    }
    out.reserve(counts.iter().sum());
    for (a, &n) in parts.chunks.iter_mut().zip(counts) {
        let c = std::mem::take(column(&mut a.chunk));
        let end = out.len() + n;
        out.extend_from_slice(&c);
        out.resize(end, T::default());
    }
    out
}

fn merge(
    mut parts: Parts,
    sources: &[Source],
    layer_names: &[String],
    layer_source: &BTreeMap<String, usize>,
    options: &Options,
) -> FeatureStore {
    // the geometry and attributes go into the store as they are, not copied
    let mut geometries: Vec<(Buf<u8>, u64)> = Vec::with_capacity(parts.chunks.len());
    let mut chunk_attrs: Vec<Buf<Attr>> = Vec::with_capacity(parts.chunks.len());
    for a in parts.chunks.iter_mut() {
        geometries.push((
            std::mem::take(&mut a.chunk.geometry),
            a.chunk.geometry_vertices,
        ));
        chunk_attrs.push(std::mem::take(&mut a.chunk.attrs));
    }
    // the keys, values and features are freed as soon as they are merged
    let chunk_keys: Vec<Interner> = parts
        .chunks
        .iter_mut()
        .map(|a| std::mem::take(&mut a.chunk.keys))
        .collect();
    let chunk_values: Vec<Interner> = parts
        .chunks
        .iter_mut()
        .map(|a| std::mem::take(&mut a.chunk.values))
        .collect();
    let chunk_features: Vec<Buf<Feature>> = parts
        .chunks
        .iter_mut()
        .map(|a| std::mem::take(&mut a.chunk.features))
        .collect();
    let feature_counts: Vec<usize> = chunk_features.iter().map(|f| f.len()).collect();
    let extents = join_sparse(&mut parts, &feature_counts, |c| &mut c.extents);
    let label_points = join_sparse(&mut parts, &feature_counts, |c| &mut c.label_points);
    let chunks: Vec<&Chunk> = parts.chunks.iter().map(|a| &a.chunk).collect();

    // Layers, by name, in the order they first appear.
    let mut layers: Vec<Layer> = Vec::new();
    let mut layer_ids: BTreeMap<String, u32> = BTreeMap::new();
    let mut stats: Vec<BTreeMap<String, Stat>> = Vec::new();
    for name in layer_names {
        layer_ids.insert(name.clone(), layers.len() as u32);
        let mut entry = tippecanoe::mbtiles::LayermapEntry::new(layers.len());
        entry.description = sources[layer_source[name]].description.clone();
        layers.push(Layer {
            name: name.clone(),
            entry,
        });
        stats.push(BTreeMap::new());
    }
    let max_samples = options.max_tilestats_sample_values;
    let mut layer_remap: Vec<Vec<u32>> = Vec::with_capacity(chunks.len());
    for c in &chunks {
        let mut m = Vec::with_capacity(c.layers.len());
        for l in &c.layers {
            let id = match layer_ids.get(&l.name) {
                Some(&id) => id,
                None => {
                    let id = layers.len() as u32;
                    layer_ids.insert(l.name.clone(), id);
                    layers.push(Layer {
                        name: l.name.clone(),
                        entry: tippecanoe::mbtiles::LayermapEntry::new(id as usize),
                    });
                    stats.push(BTreeMap::new());
                    id
                }
            };
            let e = &mut layers[id as usize].entry;
            e.points += l.points;
            e.lines += l.lines;
            e.polygons += l.polygons;
            for (k, st) in &l.stats {
                match stats[id as usize].get_mut(k) {
                    Some(have) => have.merge(st.clone(), max_samples),
                    None => {
                        stats[id as usize].insert(k.clone(), st.clone());
                    }
                }
            }
            m.push(id);
        }
        layer_remap.push(m);
    }
    for (l, st) in layers.iter_mut().zip(stats) {
        l.entry.tilestats = st.into_iter().map(|(k, v)| (k, v.to_tilestat())).collect();
    }

    let cpus = options.tippecanoe_cpus.max(1);
    let segment_layers = layer_numbers(
        &chunks,
        &parts.segment,
        layer_source,
        &layer_ids,
        layers.len(),
        cpus,
    );
    // Features go to the layer that tippecanoe's tiling names their number
    // after, while their counts stay in their own layer's statistics.
    if let Some(renamed) = renamed_layers(&segment_layers, &layers) {
        for (ci, m) in layer_remap.iter_mut().enumerate() {
            let r = &renamed[parts.segment[ci] as usize];
            for id in m.iter_mut() {
                *id = r[*id as usize];
            }
        }
    }

    let (keys, key_remap) = merge_tables(chunk_keys);
    let (values, value_remap) = merge_tables(chunk_values);

    let mut geom_off = Vec::with_capacity(chunks.len());
    let mut attr_off = Vec::with_capacity(chunks.len());
    let (mut g, mut a) = (0u64, 0u64);
    for (geom, attrs) in geometries.iter().zip(&chunk_attrs) {
        geom_off.push(g);
        attr_off.push(a);
        g += geom.0.len() as u64;
        a += attrs.len() as u64;
    }

    // the features are written straight into their places in one vector
    let total: usize = feature_counts.iter().sum();
    let mut features: Buf<Feature> = Buf::with_capacity(total);
    {
        let mut slots: Vec<&mut [std::mem::MaybeUninit<Feature>]> =
            Vec::with_capacity(chunks.len());
        let mut rest = &mut features.spare_capacity_mut()[..total];
        for &n in &feature_counts {
            let (mine, after) = std::mem::take(&mut rest).split_at_mut(n);
            slots.push(mine);
            rest = after;
        }
        let jobs: Vec<_> = slots.into_iter().zip(chunk_features).collect();
        jobs.into_par_iter()
            .enumerate()
            .for_each(|(ci, (out, chunk_features))| {
                let c = chunks[ci];
                let seq_base = parts.seq_base[ci];
                let segment = parts.segment[ci];
                let lr = &layer_remap[ci];
                for (block, fs) in out.chunks_mut(1 << 14).zip(chunk_features.chunks(1 << 14)) {
                    for (o, f) in block.iter_mut().zip(fs) {
                        o.write(Feature {
                            seq: (seq_base + f.seq as i64) as u64,
                            layer: lr[f.layer as usize],
                            geom_start: f.geom_start + geom_off[ci],
                            attr_start: f.attr_start + attr_off[ci],
                            segment,
                            ..*f
                        });
                    }
                    crate::buf::relieve();
                }
                for &i in &c.generate_ids {
                    // SAFETY: every feature of the chunk was written just above.
                    let f = unsafe { out[i as usize].assume_init_mut() };
                    f.id = f.seq + 1;
                }
            });
    }
    // SAFETY: the chunks' slices cover all `total` places, and each was written.
    unsafe { features.set_len(total) };

    let mut geometry = Geometry::default();
    for (part, vertices) in geometries {
        geometry.append(part, vertices);
    }

    chunk_attrs
        .par_iter_mut()
        .enumerate()
        .for_each(|(ci, attrs)| {
            let (kr, vr) = (&key_remap[ci], &value_remap[ci]);
            for block in attrs.chunks_mut(1 << 16) {
                for a in block {
                    a.key = kr[a.key as usize];
                    a.value = vr[a.value as usize];
                }
                crate::buf::relieve();
            }
        });
    let attr_lens: Vec<usize> = chunk_attrs.iter().map(|a| a.len()).collect();
    let mut attrs = Chunked::default();
    for part in chunk_attrs {
        attrs.append(part);
    }

    let mut bbox = [u32::MAX as i64, u32::MAX as i64, 0, 0];
    let mut bbox1: [i64; 4] = [0xFFFFFFFF, 0xFFFFFFFF, 0, 0];
    let mut bbox2: [i64; 4] = [0x1FFFFFFFF, 0xFFFFFFFF, 0x100000000, 0];
    for c in &chunks {
        for i in 0..2 {
            bbox[i] = bbox[i].min(c.file_bbox[i]);
            bbox1[i] = bbox1[i].min(c.file_bbox1[i]);
            bbox2[i] = bbox2[i].min(c.file_bbox2[i]);
        }
        for i in 2..4 {
            bbox[i] = bbox[i].max(c.file_bbox[i]);
            bbox1[i] = bbox1[i].max(c.file_bbox1[i]);
            bbox2[i] = bbox2[i].max(c.file_bbox2[i]);
        }
    }

    let (shared_nodes, shared_nodes_bloom) = if options
        .prevent
        .get(tippecanoe::options::P_SIMPLIFY_SHARED_NODES)
    {
        shared_nodes(&chunks, options.geometry_scale)
    } else {
        (Vec::new(), Vec::new())
    };

    let mut initial_x = vec![0u32; cpus];
    let mut initial_y = vec![0u32; cpus];
    let mut initialized = vec![false; cpus];
    for (ci, c) in chunks.iter().enumerate() {
        let s = parts.segment[ci] as usize;
        if let (false, Some((x, y))) = (initialized[s], c.initial) {
            initial_x[s] = x;
            initial_y[s] = y;
            initialized[s] = true;
        }
    }

    let mut store = FeatureStore {
        features,
        extents,
        label_points,
        geometry,
        geometry_scale: options.geometry_scale,
        attrs,
        keys: keys.iter().map(|k| k.s.to_string()).collect(),
        values,
        layers,
        bbox,
        bbox1,
        bbox2,
        dist_sum: parts.dist_sum,
        dist_count: parts.dist_count,
        area_sum: parts.area_sum,
        shared_nodes,
        shared_nodes_bloom,
        initial_x,
        initial_y,
        segment_layers,
        ..Default::default()
    };

    if options
        .additional
        .get(tippecanoe::options::A_VARIABLE_DEPTH_PYRAMID)
    {
        let mut spans = Vec::with_capacity(chunks.len());
        let (mut f, mut a) = (0usize, 0usize);
        for ci in 0..chunks.len() {
            spans.push(serialized::Span {
                segment: parts.segment[ci] as usize,
                features: f..f + feature_counts[ci],
                attrs: a..a + attr_lens[ci],
            });
            f += feature_counts[ci];
            a += attr_lens[ci];
        }
        serialized::layout(
            &mut store,
            &spans,
            cpus,
            tippecanoe::platform::calc_memsize() / cpus / 10,
            options.geometry_scale,
        );
    }
    store
}

/// tippecanoe's layer numbers, `numbers[segment][layer]`: each reader starts
/// with the source layers, numbered by their first source, and numbers the
/// layers it reads after those in the order it reads them. -1 for a layer
/// that the segment does not have.
fn layer_numbers(
    chunks: &[&Chunk],
    segment: &[u16],
    seeds: &BTreeMap<String, usize>,
    layer_ids: &BTreeMap<String, u32>,
    layers: usize,
    segments: usize,
) -> Vec<Vec<i64>> {
    let mut start = vec![-1i64; layers];
    for (name, &n) in seeds {
        start[layer_ids[name] as usize] = n as i64;
    }
    let mut numbers = vec![start; segments];
    let mut count = vec![seeds.len() as i64; segments];
    for (ci, c) in chunks.iter().enumerate() {
        let s = segment[ci] as usize;
        for l in &c.layers[seeds.len()..] {
            let id = layer_ids[&l.name] as usize;
            if numbers[s][id] < 0 {
                numbers[s][id] = count[s];
                count[s] += 1;
            }
        }
    }
    numbers
}

/// tippecanoe's tiling names a segment's layer number after the last of the
/// layers with that number in name order. Numbers are shared when sources
/// repeat a layer name and features name new layers. For each segment, the
/// layer that each layer's features end up in, if any segment renames any.
fn renamed_layers(numbers: &[Vec<i64>], layers: &[Layer]) -> Option<Vec<Vec<u32>>> {
    let mut out: Option<Vec<Vec<u32>>> = None;
    for (s, nums) in numbers.iter().enumerate() {
        let mut last: HashMap<i64, usize> = HashMap::new();
        for (l, &n) in nums.iter().enumerate() {
            if n >= 0 {
                let e = last.entry(n).or_insert(l);
                if layers[l].name > layers[*e].name {
                    *e = l;
                }
            }
        }
        for (l, &n) in nums.iter().enumerate() {
            if n >= 0 && last[&n] != l {
                let o = out
                    .get_or_insert_with(|| vec![(0..layers.len() as u32).collect(); numbers.len()]);
                o[s][l] = last[&n] as u32;
            }
        }
    }
    out
}

/// The nodes that tippecanoe keeps through simplification with
/// `--no-simplification-of-shared-nodes`: the ones the readers marked, and the
/// middle points of vertices whose neighbors differ between features. Sorted
/// and without duplicates, with tippecanoe's bloom filter of them.
fn shared_nodes(
    chunks: &[&Chunk],
    geometry_scale: i32,
) -> (Vec<tippecanoe::serial::Node>, Vec<u8>) {
    let mut vertices: Vec<tippecanoe::serial::Vertex> = chunks
        .iter()
        .flat_map(|c| c.vertices.iter().copied())
        .collect();
    vertices.par_sort_unstable();

    let mut nodes: Vec<u64> = chunks
        .iter()
        .flat_map(|c| c.nodes.iter().copied())
        .collect();
    let zero = tippecanoe::geometry::Draw::new(tippecanoe::geometry::VT_MOVETO, 0, 0);
    let mut prev = tippecanoe::serial::Vertex::new(zero, zero, zero);
    for v in vertices {
        if v.mid == prev.mid && (v.p1 != prev.p1 || v.p2 != prev.p2) {
            let x = v.mid.x * (1i64 << geometry_scale);
            let y = v.mid.y * (1i64 << geometry_scale);
            nodes.push(tippecanoe::projection::encode_vertex(x as u32, y as u32));
        }
        prev = v;
    }
    nodes.par_sort_unstable();
    nodes.dedup();
    // tippecanoe starts its duplicate check from ULONG_MAX
    if nodes.first() == Some(&u64::MAX) {
        nodes.clear();
    }

    let mut bloom = vec![0u8; 34567891]; // circa 34MB, size nowhere near a power of 2
    let bits = (bloom.len() * 8) as u64;
    for &n in &nodes {
        let bloom_ix = (n % bits) as usize;
        bloom[bloom_ix >> 3] |= 1 << (bloom_ix & 7);
    }

    (
        nodes
            .into_iter()
            .map(|index| tippecanoe::serial::Node { index })
            .collect(),
        bloom,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_names() {
        assert_eq!(layer_name_from_file("tests/a/in.json", 0), "in");
        assert_eq!(layer_name_from_file("x.geojson.gz", 0), "x");
        assert_eq!(layer_name_from_file("a-b c.json", 3), "abc");
        assert_eq!(layer_name_from_file("dir/", 2), "dir");
        assert_eq!(layer_name_from_file("---.json", 2), "unknown2");
    }

    #[test]
    fn segmentation() {
        let data = b"aaaa\nbbbb\ncccc\ndddd\n";
        let segs = segments(data, 0, data.len(), 4, b'\n');
        assert_eq!(segs, vec![0, 9, 14, 19, 20]);
        let unit = Unit {
            start: 0,
            end: data.len(),
            seq_base: 0,
            segment: false,
            reader: 0,
        };
        assert_eq!(
            split_unit(data, &unit, b'\n', 6),
            vec![(0, 9), (9, 19), (19, 20)]
        );
    }
}
