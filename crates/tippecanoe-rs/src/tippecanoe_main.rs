//! Port of main.hpp and main.cpp, the `tippecanoe` program: reading the input,
//! sorting the features, and handing them to tile.rs.
//!
//! The input is read by several threads at once, which the C++ connects with
//! raw pointers. Here everything those threads share is behind a shared
//! reference: each reader's files sit behind a lock that only its own thread
//! takes, the per-thread layer maps are behind locks too, and the scalars are
//! atomics. The work is split exactly as the C++ splits it.

// Kept in the shape of the C++ rather than in the shape clippy prefers.
#![allow(
    clippy::collapsible_if,
    clippy::collapsible_else_if,
    clippy::identity_op,
    clippy::if_same_then_else,
    clippy::needless_late_init,
    clippy::only_used_in_recursion,
    clippy::manual_clamp,
    clippy::manual_div_ceil,
    clippy::manual_range_contains,
    clippy::vec_init_then_push,
    clippy::int_plus_one,
    clippy::needless_range_loop,
    clippy::too_many_arguments
)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use memmap2::{Mmap, MmapMut, MmapOptions};
use rusqlite::Connection;

use crate::attribute::{AttributeOp, set_attribute_accum_arg};
use crate::cstd::{atoi, errno, perror, perror_io, set_errno, strerror, strtod, strtoll};
use crate::dirtiles::{check_dir, dir_write_metadata};
use crate::errors::*;
use crate::evaluator::{parse_filter, read_filter};
use crate::flatgeobuf::parse_flatgeobuf;
use crate::geobuf::parse_geobuf;
use crate::geocsv::parse_geocsv;
use crate::geojson::{ParseJsonArgs, json_begin_map, json_end_map, parse_json, run_parse_json};
use crate::geometry::{ClipBbox, Draw, VT_LINE, VT_MOVETO, VT_POINT, VT_POLYGON};
use crate::getopt::{Getopt, LongOption, NO_ARGUMENT, REQUIRED_ARGUMENT};
use crate::globals::*;
use crate::jsonpull::{JsonPull, JsonTree, JsonType};
use crate::mbtiles::{
    LayermapEntry, Metadata, make_metadata, mbtiles_close, mbtiles_open, mbtiles_write_metadata,
    merge_layermaps, version_str,
};
use crate::memfile::{memfile_close, memfile_open, memfile_write};
use crate::mvt::{MVT_BOOL, MVT_DOUBLE, MVT_FLOAT, MVT_INT, MVT_STRING};
use crate::options::*;
use crate::platform::{Advice, FileAt, O_CLOEXEC};
use crate::platform::{
    calc_memsize, get_max_open_files, get_null_device, get_num_avail_cpus, get_page_size,
};
use crate::pmtiles_file::{check_pmtiles, mbtiles_map_image_to_pmtiles, pmtiles_has_suffix};
use crate::pool::Stringpool;
use crate::projection::{decode_index, encode_vertex, projection, set_index_hilbert, tile2lonlat};
use crate::read_json::stringify_value;
use crate::serial::{
    NODE_SIZE, Node, Reader, SerialVal, SerializationState, StringPoolRef, VERTEX_SIZE, Vertex,
    fserialize_byte, fserialize_int, fserialize_uint, fserialize_ulong_long, fwrite_check,
};
use crate::sort::fqsort;
use crate::text::{check_utf8, format_commandline, read_unidecode};
use crate::thread::thread_create;
use crate::tile::{Strategy, manage_gap, traverse_zooms};
use crate::usage::{UsageRequiredOption, getopt_string, print_usage, strip_usage_headings};

/// `struct index`. The C++ packs `segment`, `t` and `seq` into the last 8 bytes
/// to keep the struct at 32 bytes, and so does this.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Index {
    pub start: i64,
    pub end: i64,
    pub ix: u64,
    /// segment in bits 0-15, t in bits 16-17, seq in bits 18-63
    bits: u64,
}

/// `sizeof(struct index)`
pub const INDEX_SIZE: usize = 32;

impl Index {
    pub fn segment(&self) -> i16 {
        self.bits as u16 as i16
    }

    pub fn set_segment(&mut self, segment: i16) {
        self.bits = (self.bits & !0xFFFF) | (segment as u16 as u64);
    }

    pub fn t(&self) -> u16 {
        ((self.bits >> 16) & 3) as u16
    }

    pub fn set_t(&mut self, t: u16) {
        self.bits = (self.bits & !(3 << 16)) | (((t as u64) & 3) << 16);
    }

    /// pack with segment and t to stay in 32 bytes
    pub fn seq(&self) -> u64 {
        self.bits >> 18
    }

    pub fn set_seq(&mut self, seq: u64) {
        self.bits = (self.bits & 0x3FFFF) | (seq << 18);
    }

    /// The memory image of the C++ struct, which is what the index files hold.
    pub fn to_bytes(&self) -> [u8; INDEX_SIZE] {
        let mut out = [0u8; INDEX_SIZE];
        out[0..8].copy_from_slice(&self.start.to_ne_bytes());
        out[8..16].copy_from_slice(&self.end.to_ne_bytes());
        out[16..24].copy_from_slice(&self.ix.to_ne_bytes());
        out[24..32].copy_from_slice(&self.bits.to_ne_bytes());
        out
    }

    pub fn from_bytes(b: &[u8]) -> Index {
        Index {
            start: i64::from_ne_bytes(b[0..8].try_into().unwrap()),
            end: i64::from_ne_bytes(b[8..16].try_into().unwrap()),
            ix: u64::from_ne_bytes(b[16..24].try_into().unwrap()),
            bits: u64::from_ne_bytes(b[24..32].try_into().unwrap()),
        }
    }
}

static LOW_DETAIL: AtomicI32 = AtomicI32::new(12);
static FULL_DETAIL: AtomicI32 = AtomicI32::new(-1);
static MIN_DETAIL: AtomicI32 = AtomicI32::new(7);

static SIMPLIFICATION: AtomicF64 = AtomicF64::new(1.0);
static MAXZOOM_SIMPLIFICATION: AtomicF64 = AtomicF64::new(-1.0);
static JUSTX: AtomicI64 = AtomicI64::new(-1);
static JUSTY: AtomicI64 = AtomicI64::new(-1);
static DROP_DENSER: AtomicU32 = AtomicU32::new(0);
static PRESERVE_POINT_DENSITY_THRESHOLD: AtomicU64 = AtomicU64::new(0);
static UNIDECODE_DATA: RwLock<Vec<String>> = RwLock::new(Vec::new());
static DROP_BY_ATTRIBUTE_AS_NEEDED_ATTRIBUTE: RwLock<String> = RwLock::new(String::new());
static DROP_BY_ATTRIBUTE_DESCENDING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, Default)]
struct Source {
    layer: String,
    file: String,
    description: String,
    format: String,
}

static MAX_FILES: AtomicI64 = AtomicI64::new(0);
static DISKFREE: AtomicI64 = AtomicI64::new(0);
/// `*av`, the program name
static AV: OnceLock<String> = OnceLock::new();

fn av() -> &'static str {
    AV.get().map(|s| s.as_str()).unwrap_or("tippecanoe")
}

pub fn checkdisk(r: &[Reader]) {
    let mut used: i64 = 0;
    for ri in r {
        // Pool and tree are used once.
        // Geometry and index will be duplicated during sorting and tiling.
        used += 2 * ri.geompos.load(Ordering::Relaxed)
            + 2 * ri.indexpos.load(Ordering::Relaxed)
            + ri.poolfile_off.load(Ordering::Relaxed)
            + ri.treefile_off.load(Ordering::Relaxed)
            + ri.vertexpos.load(Ordering::Relaxed)
            + ri.nodepos.load(Ordering::Relaxed);
    }

    static WARNED: AtomicI32 = AtomicI32::new(0);
    let diskfree = DISKFREE.load(Ordering::Relaxed);
    if used as f64 > diskfree as f64 * 0.9 && WARNED.load(Ordering::Relaxed) == 0 {
        eprintln!(
            "You will probably run out of disk space.\n{} bytes used or committed, of {} originally available",
            used, diskfree
        );
        WARNED.store(1, Ordering::Relaxed);
    }
}

/// The length of the C string that `s` would be: up to its first NUL.
fn cstr_len(s: &str) -> usize {
    s.bytes().position(|c| c == 0).unwrap_or(s.len())
}

/// `s` as `%s` would print it: up to its first NUL.
fn cstr(s: &str) -> &str {
    &s[..cstr_len(s)]
}

pub fn atoi_require(s: &str, what: &str) -> i32 {
    if cstr_len(s) == 0 {
        eprintln!("{}: {} must be a number (got {})", av(), what, cstr(s));
        std::process::exit(EXIT_ARGS);
    }
    let (ret, consumed) = strtoll(s, 10);
    if consumed < cstr_len(s) {
        eprintln!("{}: {} must be a number (got {})", av(), what, cstr(s));
        std::process::exit(EXIT_ARGS);
    }
    ret as i32
}

pub fn atof_require(s: &str, what: &str) -> f64 {
    if cstr_len(s) == 0 {
        eprintln!("{}: {} must be a number (got {})", av(), what, cstr(s));
        std::process::exit(EXIT_ARGS);
    }
    let (ret, consumed) = strtod(s);
    if consumed < cstr_len(s) {
        eprintln!("{}: {} must be a number (got {})", av(), what, cstr(s));
        std::process::exit(EXIT_ARGS);
    }
    ret
}

pub fn atoll_require(s: &str, what: &str) -> i64 {
    if cstr_len(s) == 0 {
        eprintln!("{}: {} must be a number (got {})", av(), what, cstr(s));
        std::process::exit(EXIT_ARGS);
    }
    let (ret, consumed) = strtoll(s, 10);
    if consumed < cstr_len(s) {
        eprintln!("{}: {} must be a number (got {})", av(), what, cstr(s));
        std::process::exit(EXIT_ARGS);
    }
    ret
}

/// `open(path, flags)`, returning the descriptor or -1 with errno set.
fn open_fd(path: &str, flags: i32, mode: i32) -> i32 {
    crate::platform::open(path, flags, mode)
}

fn close_fd(fd: i32) -> i32 {
    // SAFETY: closes a descriptor this module owns.
    unsafe { libc::close(fd) }
}

fn unlink(path: &str) -> i32 {
    crate::platform::unlink(path)
}

/// `fstat(fd).st_size`, or `Err(errno)`.
fn fstat_size(fd: i32) -> Result<i64, i32> {
    // SAFETY: st is a valid stat buffer to fill in.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: fstat of a descriptor into a valid buffer.
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        Err(errno())
    } else {
        Ok(st.st_size as i64)
    }
}

/// A `File` for a descriptor that something else owns and will close.
fn borrow_fd(fd: i32) -> ManuallyDrop<File> {
    crate::platform::borrow_fd(fd)
}

/// `mmap(NULL, len, PROT_READ, MAP_PRIVATE, fd, 0)`. Fails for a zero length,
/// as mmap does.
fn mmap_fd(fd: i32, len: usize) -> std::io::Result<Mmap> {
    if len == 0 {
        return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
    }
    // SAFETY: the temporary files mapped here are not changed while mapped.
    unsafe {
        MmapOptions::new()
            .len(len)
            .map_copy_read_only(&*borrow_fd(fd))
    }
}

/// `madvise`, whose result the C++ ignores.
fn madvise(map: &Mmap, advice: Advice) {
    crate::platform::madvise(map, advice);
}

/// `fwrite(buf, size, count, fp) != count`, for the writes that the C++ checks
/// with plain fwrite.
fn fwrite_ok(fp: &mut dyn Write, buf: &[u8]) -> bool {
    fp.write_all(buf).is_ok()
}

/// `rewind(fp)` on a file opened "w+b" that has been written.
fn rewind(fp: BufWriter<File>) -> BufReader<File> {
    let mut f = match fp.into_inner() {
        Ok(f) => f,
        Err(e) => e.into_inner().into_parts().0,
    };
    let _ = f.seek(SeekFrom::Start(0));
    BufReader::new(f)
}

/// `fread(p, size, 1, fp)`: true if a whole record was read.
fn fread_record(fp: &mut dyn Read, p: &mut [u8]) -> bool {
    let mut got = 0;
    while got < p.len() {
        match fp.read(&mut p[got..]) {
            Ok(0) => return false,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
    true
}

pub fn init_cpus() {
    let cpus: usize = match std::env::var("TIPPECANOE_MAX_THREADS") {
        Ok(v) => atoi_require(&v, "TIPPECANOE_MAX_THREADS") as usize,
        Err(_) => get_num_avail_cpus() as usize,
    };
    let mut cpus = cpus;

    if cpus < 1 {
        cpus = 1;
    }

    // Guard against short struct index.segment
    if cpus > 32767 {
        cpus = 32767;
    }

    // Round down to a power of 2
    cpus = 1 << ((cpus as f64).ln() / 2f64.ln()) as i32;
    CPUS.store(cpus, Ordering::Relaxed);

    let mut max_files = get_max_open_files() as i64;

    // Don't really want too many temporary files, because the file system
    // will start to bog down eventually
    if max_files > 2000 {
        max_files = 2000;
    }

    // MacOS can run out of system file descriptors
    // even if we stay under the rlimit, so try to
    // find out the real limit.
    let mut fds: Vec<i32> = vec![0; max_files as usize];
    let mut i: i64 = 0;
    while i < max_files {
        fds[i as usize] = open_fd(get_null_device(), libc::O_RDONLY | O_CLOEXEC, 0);
        if fds[i as usize] < 0 {
            break;
        }
        i += 1;
    }
    for fd in fds.iter().take(i as usize) {
        if close_fd(*fd) < 0 {
            perror("close");
            std::process::exit(EXIT_CLOSE);
        }
    }

    // Scale down because we really don't want to run the system out of files
    max_files = i * 3 / 4;
    if max_files < 32 {
        eprintln!("Can't open a useful number of files: {}", max_files);
        std::process::exit(EXIT_OPEN);
    }
    MAX_FILES.store(max_files, Ordering::Relaxed);

    let mut temp_files = ((max_files - 10) / 2) as usize;
    if temp_files > cpus * 4 {
        temp_files = cpus * 4;
    }
    TEMP_FILES.store(temp_files, Ordering::Relaxed);
}

pub fn indexcmp(i1: &Index, i2: &Index) -> i32 {
    if i1.ix < i2.ix {
        return -1;
    } else if i1.ix > i2.ix {
        return 1;
    }

    if i1.seq() < i2.seq() {
        return -1;
    } else if i1.seq() > i2.seq() {
        return 1;
    }

    0
}

/// `struct mergelist`. `next` is an index into the array of mergelists.
#[derive(Clone, Copy, Debug, Default)]
struct Mergelist {
    start: i64,
    end: i64,

    next: Option<usize>,
}

fn index_at(map: &[u8], off: i64) -> Index {
    Index::from_bytes(&map[off as usize..off as usize + INDEX_SIZE])
}

fn insert(merges: &mut [Mergelist], m: usize, head: &mut Option<usize>, map: &[u8]) {
    // `prev` is the link being followed: None for `head`, otherwise the
    // `next` of that mergelist.
    let mut prev: Option<usize> = None;
    let mut cur = *head;
    while let Some(h) = cur {
        if indexcmp(
            &index_at(map, merges[m].start),
            &index_at(map, merges[h].start),
        ) > 0
        {
            prev = Some(h);
            cur = merges[h].next;
        } else {
            break;
        }
    }

    merges[m].next = cur;
    match prev {
        None => *head = Some(m),
        Some(p) => merges[p].next = Some(m),
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct DropState {
    gap: f64,
    previndex: u64,
    interval: f64,
    /// floating point because interval is
    seq: f64,
}

#[derive(Clone, Copy, Debug, Default)]
struct DropDensest {
    gap: u64,
    seq: usize,
}

fn calc_feature_minzoom(ix: &Index, ds: &mut [DropState], maxzoom: i32, gamma: f64) -> i32 {
    let mut feature_minzoom: i32 = 0;

    let t = ix.t() as i32;
    if gamma >= 0.0
        && (t == VT_POINT
            || (additional(A_LINE_DROP) != 0 && t == VT_LINE)
            || (additional(A_POLYGON_DROP) != 0 && t == VT_POLYGON))
    {
        let mut i = maxzoom as isize;
        while i >= 0 {
            ds[i as usize].seq += 1.0;
            i -= 1;
        }
        let mut i = maxzoom as isize;
        while i >= 0 {
            if ds[i as usize].seq < 0.0 {
                feature_minzoom = (i + 1) as i32;

                // The feature we are pushing out
                // appears in zooms i + 1 through maxzoom,
                // so track where that was so we can make sure
                // not to cluster something else that is *too*
                // far away into it.
                for j in (i + 1)..=(maxzoom as isize) {
                    ds[j as usize].previndex = ix.ix;
                }

                break;
            } else {
                ds[i as usize].seq -= ds[i as usize].interval;
            }
            i -= 1;
        }

        // If this feature has been chosen only for a high zoom level,
        // check whether at a low zoom level it is nevertheless too far
        // from the last feature chosen for that low zoom, in which case
        // we will go ahead and push it out.

        let preserve_point_density_threshold =
            PRESERVE_POINT_DENSITY_THRESHOLD.load(Ordering::Relaxed);
        if preserve_point_density_threshold > 0 {
            let mut i: isize = 0;
            while i < feature_minzoom as isize && i < maxzoom as isize {
                let a = ((1i64 << (32 - i)) as u64) / preserve_point_density_threshold;
                if ix.ix.wrapping_sub(ds[i as usize].previndex) > a.wrapping_mul(a) {
                    feature_minzoom = i as i32;

                    for j in i..=(maxzoom as isize) {
                        ds[j as usize].previndex = ix.ix;
                    }

                    break;
                }
                i += 1;
            }
        }

        // XXX manage_gap
    }

    feature_minzoom
}

#[allow(clippy::too_many_arguments)]
fn merge(
    merges: &mut [Mergelist],
    nmerges: usize,
    map: &[u8],
    indexfile: &mut dyn Write,
    bytes: i32,
    geom_map: &[u8],
    geom_out: &mut dyn Write,
    geompos: &AtomicI64,
    progress: &mut i64,
    progress_max: &mut i64,
    progress_reported: &mut i64,
    maxzoom: i32,
    gamma: f64,
    ds: &mut [DropState],
) {
    let mut head: Option<usize> = None;

    for i in 0..nmerges {
        if merges[i].start < merges[i].end {
            insert(merges, i, &mut head, map);
        }
    }

    LAST_PROGRESS.store(0.0);

    while let Some(h) = head {
        let mut ix = index_at(map, merges[h].start);
        let pos = geompos.load(Ordering::Relaxed);

        // MAGIC: This knows that the feature minzoom is the last byte of the serialized feature
        // and is writing one byte less and then adding the byte for the minzoom.

        let len = (ix.end - ix.start - 1) as usize;
        fwrite_check(
            &geom_map[ix.start as usize..ix.start as usize + len],
            1,
            len,
            geom_out,
            geompos,
            "merge geometry",
        );
        let feature_minzoom = calc_feature_minzoom(&ix, ds, maxzoom, gamma);
        fserialize_byte(geom_out, feature_minzoom as i8, geompos, "merge geometry");

        // Count this as an 75%-accomplishment, since we already 25%-counted it
        *progress += (ix.end - ix.start) * 3 / 4;
        if get(&QUIET) == 0
            && get(&QUIET_PROGRESS) == 0
            && progress_time()
            && 100 * *progress / *progress_max != *progress_reported
        {
            eprint!(
                "Reordering geometry: {}% \r",
                100 * *progress / *progress_max
            );
            *progress_reported = 100 * *progress / *progress_max;
        }

        ix.start = pos;
        ix.end = geompos.load(Ordering::Relaxed);
        let indexpos = AtomicI64::new(0);
        fwrite_check(
            &ix.to_bytes(),
            bytes as usize,
            1,
            indexfile,
            &indexpos,
            "merge temporary",
        );
        merges[h].start += bytes as i64;

        let m = h;
        head = merges[m].next;
        merges[m].next = None;

        if merges[m].start < merges[m].end {
            insert(merges, m, &mut head, map);
        }
    }
}

#[allow(dead_code)]
struct SortArg {
    task: i32,
    cpus: i32,
    indexpos: i64,
    indexfd: i32,
    nmerges: usize,
    unit: i64,
    bytes: i32,
}

impl SortArg {
    #[allow(clippy::too_many_arguments)]
    fn new(
        task1: i32,
        cpus1: i32,
        indexpos1: i64,
        indexfd1: i32,
        nmerges1: usize,
        unit1: i64,
        bytes1: i32,
    ) -> SortArg {
        SortArg {
            task: task1,
            cpus: cpus1,
            indexpos: indexpos1,
            indexfd: indexfd1,
            nmerges: nmerges1,
            unit: unit1,
            bytes: bytes1,
        }
    }
}

/// Sorts this task's chunks of the index file in place. Returns the
/// mergelists it filled in, by their position in the array of them.
fn run_sort(a: &SortArg) -> Vec<(usize, Mergelist)> {
    let mut out = Vec::new();
    let file = borrow_fd(a.indexfd);

    let mut start = a.task as i64 * a.unit;
    while start < a.indexpos {
        let mut end = start + a.unit;
        if end > a.indexpos {
            end = a.indexpos;
        }

        out.push((
            (start / a.unit) as usize,
            Mergelist {
                start,
                end,
                next: None,
            },
        ));

        // Read section of index into memory to sort and then use pwrite()
        // to write it back out rather than sorting in mapped memory,
        // because writable mapped memory seems to have bad performance
        // problems on ECS (and maybe in containers in general)?

        let mut s = vec![0u8; (end - start) as usize];

        match file.read_at(&mut s, start as u64) {
            Ok(n) if n as i64 == end - start => {}
            Ok(_) => {
                eprintln!("pread(index): {}", strerror(0));
                std::process::exit(EXIT_READ);
            }
            Err(e) => {
                eprintln!(
                    "pread(index): {}",
                    strerror(e.raw_os_error().unwrap_or(libc::EIO))
                );
                std::process::exit(EXIT_READ);
            }
        }

        let bytes = a.bytes as usize;
        let mut recs: Vec<Index> = s.chunks_exact(bytes).map(Index::from_bytes).collect();
        recs.sort_by(|x, y| indexcmp(x, y).cmp(&0));
        for (i, r) in recs.iter().enumerate() {
            s[i * bytes..(i + 1) * bytes].copy_from_slice(&r.to_bytes());
        }

        match file.write_at(&s, start as u64) {
            Ok(n) if n as i64 == end - start => {}
            Ok(_) => {
                eprintln!("pwrite(index): {}", strerror(0));
                std::process::exit(EXIT_WRITE);
            }
            Err(e) => {
                eprintln!(
                    "pwrite(index): {}",
                    strerror(e.raw_os_error().unwrap_or(libc::EIO))
                );
                std::process::exit(EXIT_WRITE);
            }
        }

        start += a.unit * a.cpus as i64;
    }

    out
}

/// What the input threads add up for guessing the maxzoom.
#[derive(Clone, Copy, Debug, Default)]
struct DistSums {
    dist_sum: f64,
    dist_count: usize,
    area_sum: f64,
}

/// What every input thread shares, which the C++ passes as separate pointers.
struct ReadShared<'a> {
    readers: &'a [Reader],
    progress_seq: &'a AtomicI64,
    exclude: &'a BTreeSet<String>,
    include: &'a BTreeSet<String>,
    exclude_all: i32,
    layermaps: &'a [Mutex<BTreeMap<String, LayermapEntry>>],
    initialized: &'a [AtomicI32],
    initial_x: &'a [AtomicU32],
    initial_y: &'a [AtomicU32],
    attribute_types: &'a HashMap<String, i32>,
}

impl<'a> ReadShared<'a> {
    /// The serialization state of input thread `i`, which the C++ sets up field by field.
    #[allow(clippy::too_many_arguments)]
    fn sst(
        &self,
        i: usize,
        fname: &'a str,
        layer_seq: i64,
        want_dist: bool,
        maxzoom: i32,
        uses_gamma: bool,
        filters: bool,
        basezoom: i32,
    ) -> SerializationState<'a> {
        SerializationState {
            fname,
            line: 0,
            layer_seq,
            progress_seq: self.progress_seq,
            readers: self.readers,
            segment: i as i32,
            initialized: &self.initialized[i],
            initial_x: &self.initial_x[i],
            initial_y: &self.initial_y[i],
            dist_sum: 0.0,
            dist_count: 0,
            area_sum: 0.0,
            want_dist,
            maxzoom,
            uses_gamma,
            filters,
            layermap: &self.layermaps[i],
            exclude: self.exclude,
            include: self.include,
            exclude_all: self.exclude_all,
            basezoom,
            attribute_types: self.attribute_types,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn do_read_parallel(
    map: &[u8],
    len: i64,
    initial_offset: i64,
    reading: &str,
    shared: &ReadShared<'_>,
    basezoom: i32,
    source: i32,
    maxzoom: i32,
    layername: &str,
    uses_gamma: bool,
    separator: i32,
    dist: &Mutex<DistSums>,
    want_dist: bool,
    filters: bool,
) {
    let cpus = get(&CPUS);
    let mut segs: Vec<i64> = vec![0; cpus + 1];
    segs[0] = 0;
    segs[cpus] = len;

    for i in 1..cpus {
        segs[i] = len * i as i64 / cpus as i64;

        while segs[i] < len && map[segs[i] as usize] as i32 != separator {
            segs[i] += 1;
        }
    }

    let mut sst: Vec<SerializationState<'_>> = Vec::with_capacity(cpus);
    for i in 0..cpus {
        // To preserve feature ordering, unique id for each segment
        // begins with that segment's offset into the input
        sst.push(shared.sst(
            i,
            reading,
            segs[i] + initial_offset,
            want_dist,
            maxzoom,
            uses_gamma,
            filters,
            basezoom,
        ));
    }

    std::thread::scope(|scope| {
        let mut pthreads = Vec::with_capacity(cpus);
        for (i, st) in sst.iter_mut().enumerate() {
            let seg = &map[segs[i] as usize..segs[i + 1] as usize];
            pthreads.push(thread_create(scope, move || {
                let jp = json_begin_map(seg);
                let mut pja = ParseJsonArgs::new(jp, source, layername, st);
                run_parse_json(&mut pja);
                json_end_map(pja.jp);
            }));
        }

        for h in pthreads {
            if let Err(e) = h.join() {
                std::panic::resume_unwind(e);
            }
        }
    });

    let mut d = dist.lock().unwrap();
    for st in &sst {
        d.dist_sum += st.dist_sum;
        d.dist_count += st.dist_count;
        d.area_sum += st.area_sum;
    }
}

/// `STREAM`: an input file read through stdio or through zlib.
struct Stream {
    fp: BufReader<StreamSource>,
}

enum StreamSource {
    File(File),
    Gz(GzFile),
}

/// `gzFile` opened for reading: gzip data is decompressed, and anything else is
/// read as it is, as zlib does.
struct GzFile {
    inner: GzInner,
}

enum GzInner {
    Unknown(Option<File>),
    Direct(BufReader<File>),
    Gzip(Box<flate2::bufread::MultiGzDecoder<BufReader<File>>>),
}

impl Read for GzFile {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if let GzInner::Unknown(f) = &mut self.inner {
            let mut r = BufReader::new(f.take().unwrap());
            let head = r.fill_buf()?;
            let is_gzip = head.len() >= 2 && head[0] == 0x1F && head[1] == 0x8B;
            self.inner = if is_gzip {
                GzInner::Gzip(Box::new(flate2::bufread::MultiGzDecoder::new(r)))
            } else {
                GzInner::Direct(r)
            };
        }

        match &mut self.inner {
            GzInner::Direct(r) => r.read(out),
            GzInner::Gzip(r) => r.read(out),
            GzInner::Unknown(_) => unreachable!(),
        }
    }
}

impl Read for StreamSource {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        match self {
            StreamSource::File(f) => {
                // fread returns a short count on an error
                match f.read(out) {
                    Ok(n) => Ok(n),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Err(e),
                    Err(_) => Ok(0),
                }
            }
            StreamSource::Gz(gz) => match gz.read(out) {
                Ok(n) => Ok(n),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Err(e),
                Err(_) => {
                    eprintln!("{}: Error reading compressed data", av());
                    std::process::exit(EXIT_READ);
                }
            },
        }
    }
}

impl Stream {
    fn fclose(self) -> i32 {
        drop(self);
        0
    }

    fn peekc(&mut self) -> i32 {
        match self.fp.fill_buf() {
            Ok(b) if !b.is_empty() => b[0] as i32,
            _ => libc::EOF,
        }
    }

    /// Reads up to `out.len()` bytes, as many as there are, like `fread`.
    fn read(&mut self, out: &mut [u8]) -> usize {
        let mut got = 0;
        while got < out.len() {
            match self.fp.read(&mut out[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        got
    }

    fn json_begin(&mut self) -> JsonPull<'_> {
        JsonPull::begin_file(Box::new(&mut self.fp))
    }
}

fn streamfdopen(fd: i32, _mode: &str, fname: &str) -> Option<Stream> {
    // SAFETY: the stream takes over the descriptor and closes it.
    let file = unsafe { crate::platform::file_from_fd(fd) };

    let source = if fname.len() > 3 && fname.ends_with(".gz") {
        StreamSource::Gz(GzFile {
            inner: GzInner::Unknown(Some(file)),
        })
    } else {
        StreamSource::File(file)
    };

    Some(Stream {
        fp: BufReader::new(source),
    })
}

/// The temporary file that a chunk of streamed input was copied to, which
/// the C++ wraps in a STREAM only so that it can be closed.
struct ReadTemp {
    fp: BufWriter<File>,
}

impl ReadTemp {
    fn fclose(self) -> i32 {
        match self.fp.into_inner() {
            Ok(_) => 0,
            Err(_) => -1,
        }
    }
}

fn streamfpopen(fp: BufWriter<File>) -> ReadTemp {
    ReadTemp { fp }
}

struct ReadParallelArg<'a> {
    fd: i32,
    fp: ReadTemp,
    offset: i64,
    len: i64,
    is_parsing: &'a AtomicI32,
    separator: i32,

    reading: &'a str,
    shared: &'a ReadShared<'a>,
    maxzoom: i32,
    basezoom: i32,
    source: i32,
    layername: String,
    uses_gamma: bool,
    dist: &'a Mutex<DistSums>,
    want_dist: bool,
    filters: bool,
}

fn run_read_parallel(rpa: ReadParallelArg<'_>) {
    let mut rpa = rpa;

    match fstat_size(rpa.fd) {
        Err(e) => {
            set_errno(e);
            perror("stat read temp");
        }
        Ok(size) => {
            if rpa.len != size {
                eprintln!(
                    "wrong number of bytes in temporary: {} vs {}",
                    rpa.len, size
                );
            }
            rpa.len = size;
        }
    }

    let map = match mmap_fd(rpa.fd, rpa.len as usize) {
        Ok(m) => m,
        Err(e) => {
            perror_io("map intermediate input", &e);
            std::process::exit(EXIT_MEMORY);
        }
    };
    madvise(&map, Advice::Random); // sequential, but from several pointers at once

    do_read_parallel(
        &map,
        rpa.len,
        rpa.offset,
        rpa.reading,
        rpa.shared,
        rpa.basezoom,
        rpa.source,
        rpa.maxzoom,
        &rpa.layername,
        rpa.uses_gamma,
        rpa.separator,
        rpa.dist,
        rpa.want_dist,
        rpa.filters,
    );

    drop(map);
    if rpa.fp.fclose() != 0 {
        perror("close source file");
        std::process::exit(EXIT_CLOSE);
    }
    crate::platform::close_shared_fd(rpa.fd);

    rpa.is_parsing.store(0, Ordering::SeqCst);
}

/// This has to kick off an intermediate thread to start the parser threads,
/// so the main thread can get back to reading the next input stage while
/// the intermediate thread waits for the completion of the parser threads.
fn start_parsing<'scope, 'env>(
    scope: &'scope std::thread::Scope<'scope, 'env>,
    rpa: ReadParallelArg<'env>,
    parallel_parser: &mut Option<std::thread::ScopedJoinHandle<'scope, ()>>,
    parser_created: &mut bool,
) {
    rpa.is_parsing.store(1, Ordering::SeqCst);

    *parallel_parser = Some(thread_create(scope, move || run_read_parallel(rpa)));
    *parser_created = true;
}

/// `pthread_join(parallel_parser)`
fn join_parser(parallel_parser: &mut Option<std::thread::ScopedJoinHandle<'_, ()>>) {
    if let Some(h) = parallel_parser.take() {
        if let Err(e) = h.join() {
            std::panic::resume_unwind(e);
        }
    }
}

/// A temporary file: `name` is the template, and on return the name of the
/// file, which has been unlinked. Returns the descriptor and a buffered
/// writer on a second descriptor for it, as `fopen_oflag` gives.
fn temp_pair(name: &mut String, open_failure_code: i32) -> (i32, BufWriter<File>) {
    let fd = mkstemp_cloexec(name);
    if fd < 0 {
        perror(name);
        std::process::exit(open_failure_code);
    }
    let fp = match fopen_oflag(name, "wb", libc::O_WRONLY | O_CLOEXEC) {
        Some(f) => BufWriter::new(f),
        None => {
            perror(name);
            std::process::exit(EXIT_OPEN);
        }
    };
    (fd, fp)
}

#[allow(clippy::too_many_arguments)]
fn radix1(
    geomfds_in: &[i32],
    indexfds_in: &[i32],
    inputs: i32,
    prefix: i32,
    splits: i32,
    mem: i64,
    tmpdir: &str,
    availfiles: &mut i64,
    geomfile: &mut BufWriter<File>,
    indexfile: &mut BufWriter<File>,
    geompos_out: &AtomicI64,
    progress: &mut i64,
    progress_max: &mut i64,
    progress_reported: &mut i64,
    maxzoom: i32,
    basezoom: i32,
    droprate: f64,
    gamma: f64,
    ds: &mut [DropState],
) {
    // Arranged as bits to facilitate subdividing again if a subdivided file is still huge.
    //
    // There must be at least two buckets. With only one, each subdivision would
    // consume no bits of the index, so it would never reach the maximum prefix
    // that stops the recursion, and the shift that chooses a feature's bucket
    // below would be by the full width of the index. That shift is undefined,
    // and what it does in practice is to mask the shift count down to zero, so
    // the bucket number comes out as the whole shifted index instead of as 0
    // and the writes are made through whatever is found beyond the end of the
    // arrays of buckets.
    let mut splitbits: i32 = 1;
    if splits > 1 {
        splitbits = ((splits as f64).ln() / 2f64.ln()) as i32;
    }
    let splits: i32 = 1 << splitbits;

    let mut geomfiles: Vec<Option<BufWriter<File>>> = Vec::with_capacity(splits as usize);
    let mut indexfiles: Vec<Option<BufWriter<File>>> = Vec::with_capacity(splits as usize);
    let mut geomfds: Vec<i32> = vec![0; splits as usize];
    let mut indexfds: Vec<i32> = vec![0; splits as usize];
    let sub_geompos: Vec<AtomicI64> = (0..splits).map(|_| AtomicI64::new(0)).collect();

    for i in 0..splits as usize {
        sub_geompos[i].store(0, Ordering::Relaxed);

        let mut geomname = format!("{}/geom.XXXXXXXX", tmpdir);
        let mut indexname = format!("{}/index.XXXXXXXX", tmpdir);

        geomfds[i] = mkstemp_cloexec(&mut geomname);
        if geomfds[i] < 0 {
            perror(&geomname);
            std::process::exit(EXIT_OPEN);
        }
        indexfds[i] = mkstemp_cloexec(&mut indexname);
        if indexfds[i] < 0 {
            perror(&indexname);
            std::process::exit(EXIT_OPEN);
        }

        match fopen_oflag(&geomname, "wb", libc::O_WRONLY | O_CLOEXEC) {
            Some(f) => geomfiles.push(Some(BufWriter::new(f))),
            None => {
                perror(&geomname);
                std::process::exit(EXIT_OPEN);
            }
        }
        match fopen_oflag(&indexname, "wb", libc::O_WRONLY | O_CLOEXEC) {
            Some(f) => indexfiles.push(Some(BufWriter::new(f))),
            None => {
                perror(&indexname);
                std::process::exit(EXIT_OPEN);
            }
        }

        *availfiles -= 4;

        unlink(&geomname);
        unlink(&indexname);
    }

    for i in 0..inputs as usize {
        let geomst_size = match fstat_size(geomfds_in[i]) {
            Ok(s) => s,
            Err(e) => {
                set_errno(e);
                perror("stat geom");
                std::process::exit(EXIT_STAT);
            }
        };
        let indexst_size = match fstat_size(indexfds_in[i]) {
            Ok(s) => s,
            Err(e) => {
                set_errno(e);
                perror("stat index");
                std::process::exit(EXIT_STAT);
            }
        };

        if indexst_size != 0 {
            let indexmap = match mmap_fd(indexfds_in[i], indexst_size as usize) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("fd {}, len {}", indexfds_in[i] as i64, indexst_size);
                    perror_io("map index", &e);
                    std::process::exit(EXIT_STAT);
                }
            };
            madvise(&indexmap, Advice::Sequential);
            madvise(&indexmap, Advice::WillNeed);
            let geommap = match mmap_fd(geomfds_in[i], geomst_size as usize) {
                Ok(m) => m,
                Err(e) => {
                    perror_io("map geom", &e);
                    std::process::exit(EXIT_MEMORY);
                }
            };
            madvise(&geommap, Advice::Sequential);
            madvise(&geommap, Advice::WillNeed);

            for a in 0..indexst_size as usize / INDEX_SIZE {
                let mut ix = Index::from_bytes(&indexmap[a * INDEX_SIZE..(a + 1) * INDEX_SIZE]);
                let which = ((ix.ix << prefix) >> (64 - splitbits)) as usize;
                let pos = sub_geompos[which].load(Ordering::Relaxed);

                let len = (ix.end - ix.start) as usize;
                fwrite_check(
                    &geommap[ix.start as usize..ix.start as usize + len],
                    len,
                    1,
                    geomfiles[which].as_mut().unwrap(),
                    &sub_geompos[which],
                    "geom",
                );

                // Count this as a 25%-accomplishment, since we will copy again
                *progress += (ix.end - ix.start) / 4;
                if get(&QUIET) == 0
                    && get(&QUIET_PROGRESS) == 0
                    && progress_time()
                    && 100 * *progress / *progress_max != *progress_reported
                {
                    eprint!(
                        "Reordering geometry: {}% \r",
                        100 * *progress / *progress_max
                    );
                    *progress_reported = 100 * *progress / *progress_max;
                }

                ix.start = pos;
                ix.end = sub_geompos[which].load(Ordering::Relaxed);

                let indexpos = AtomicI64::new(0);
                fwrite_check(
                    &ix.to_bytes(),
                    INDEX_SIZE,
                    1,
                    indexfiles[which].as_mut().unwrap(),
                    &indexpos,
                    "index",
                );
            }

            drop(indexmap);
            drop(geommap);
        }

        if close_fd(geomfds_in[i]) < 0 {
            perror("close geom");
            std::process::exit(EXIT_CLOSE);
        }
        if close_fd(indexfds_in[i]) < 0 {
            perror("close index");
            std::process::exit(EXIT_CLOSE);
        }

        *availfiles += 2;
    }

    for i in 0..splits as usize {
        if let Err(e) = geomfiles[i].take().unwrap().into_inner() {
            perror_io("fclose geom", e.error());
            std::process::exit(EXIT_CLOSE);
        }
        if let Err(e) = indexfiles[i].take().unwrap().into_inner() {
            perror_io("fclose index", e.error());
            std::process::exit(EXIT_CLOSE);
        }

        *availfiles += 2;
    }

    for i in 0..splits as usize {
        let mut already_closed = false;

        let geomst_size = match fstat_size(geomfds[i]) {
            Ok(s) => s,
            Err(e) => {
                set_errno(e);
                perror("stat geom");
                std::process::exit(EXIT_STAT);
            }
        };
        let indexst_size = match fstat_size(indexfds[i]) {
            Ok(s) => s,
            Err(e) => {
                set_errno(e);
                perror("stat index");
                std::process::exit(EXIT_STAT);
            }
        };

        if indexst_size > 0 {
            // Subdividing again would only make progress if the next level could
            // split into at least two buckets, which it can't if there are no longer
            // enough files left to do it with. In that case, sort in memory instead,
            // even though this is more memory than we wanted to use at once, since
            // it is the only way left to get this bucket sorted.
            let can_subdivide = *availfiles / 4 > 1;

            if indexst_size + geomst_size < mem || !can_subdivide {
                let indexpos: i64 = indexst_size;
                let bytes = INDEX_SIZE as i32;
                let cpus = get(&CPUS);

                let page = get_page_size();
                // Don't try to sort more than 2GB at once,
                // which used to crash Macs and may still
                let max_unit: i64 = 2 * 1024 * 1024 * 1024;
                let mut unit: i64 = ((indexpos as u64 / cpus as u64 + bytes as u64 - 1)
                    / bytes as u64
                    * bytes as u64) as i64;
                if unit > max_unit {
                    unit = max_unit;
                }
                unit = (unit + page - 1) / page * page;
                if unit < page {
                    unit = page;
                }

                let nmerges = ((indexpos + unit - 1) / unit) as usize;
                let mut merges: Vec<Mergelist> = vec![Mergelist::default(); nmerges];

                for m in merges.iter_mut() {
                    m.start = 0;
                    m.end = 0;
                }

                let mut args: Vec<SortArg> = Vec::new();

                for a in 0..cpus {
                    args.push(SortArg::new(
                        a as i32,
                        cpus as i32,
                        indexpos,
                        indexfds[i],
                        nmerges,
                        unit,
                        bytes,
                    ));
                }

                let results: Vec<Vec<(usize, Mergelist)>> = std::thread::scope(|scope| {
                    let pthreads: Vec<_> = args
                        .iter()
                        .map(|arg| thread_create(scope, move || run_sort(arg)))
                        .collect();

                    pthreads
                        .into_iter()
                        .map(|h| match h.join() {
                            Ok(r) => r,
                            Err(e) => std::panic::resume_unwind(e),
                        })
                        .collect()
                });
                for r in results {
                    for (at, m) in r {
                        merges[at] = m;
                    }
                }

                let indexmap = match mmap_fd(indexfds[i], indexst_size as usize) {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("fd {}, len {}", indexfds[i] as i64, indexst_size);
                        perror_io("map index", &e);
                        std::process::exit(EXIT_MEMORY);
                    }
                };
                madvise(&indexmap, Advice::Random); // sequential, but from several pointers at once
                madvise(&indexmap, Advice::WillNeed);
                let geommap = match mmap_fd(geomfds[i], geomst_size as usize) {
                    Ok(m) => m,
                    Err(e) => {
                        perror_io("map geom", &e);
                        std::process::exit(EXIT_MEMORY);
                    }
                };
                madvise(&geommap, Advice::Random);
                madvise(&geommap, Advice::WillNeed);

                merge(
                    &mut merges,
                    nmerges,
                    &indexmap,
                    indexfile,
                    bytes,
                    &geommap,
                    geomfile,
                    geompos_out,
                    progress,
                    progress_max,
                    progress_reported,
                    maxzoom,
                    gamma,
                    ds,
                );

                drop(indexmap);
                drop(geommap);
            } else if indexst_size == INDEX_SIZE as i64 || prefix + splitbits >= 64 {
                let indexmap = match mmap_fd(indexfds[i], indexst_size as usize) {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("fd {}, len {}", indexfds[i] as i64, indexst_size);
                        perror_io("map index", &e);
                        std::process::exit(EXIT_MEMORY);
                    }
                };
                madvise(&indexmap, Advice::Sequential);
                madvise(&indexmap, Advice::WillNeed);
                let geommap = match mmap_fd(geomfds[i], geomst_size as usize) {
                    Ok(m) => m,
                    Err(e) => {
                        perror_io("map geom", &e);
                        std::process::exit(EXIT_MEMORY);
                    }
                };
                madvise(&geommap, Advice::Random);
                madvise(&geommap, Advice::WillNeed);

                for a in 0..indexst_size as usize / INDEX_SIZE {
                    let mut ix = Index::from_bytes(&indexmap[a * INDEX_SIZE..(a + 1) * INDEX_SIZE]);
                    let pos = geompos_out.load(Ordering::Relaxed);

                    // MAGIC: This knows that the feature minzoom is the last byte of the serialized feature
                    // and is writing one byte less and then adding the byte for the minzoom,
                    // the same as merge() does.

                    let len = (ix.end - ix.start - 1) as usize;
                    fwrite_check(
                        &geommap[ix.start as usize..ix.start as usize + len],
                        1,
                        len,
                        geomfile,
                        geompos_out,
                        "geom",
                    );
                    let feature_minzoom = calc_feature_minzoom(&ix, ds, maxzoom, gamma);
                    fserialize_byte(
                        geomfile,
                        feature_minzoom as i8,
                        geompos_out,
                        "merge geometry",
                    );

                    // Count this as an 75%-accomplishment, since we already 25%-counted it
                    *progress += (ix.end - ix.start) * 3 / 4;
                    if get(&QUIET) == 0
                        && get(&QUIET_PROGRESS) == 0
                        && progress_time()
                        && 100 * *progress / *progress_max != *progress_reported
                    {
                        eprint!(
                            "Reordering geometry: {}% \r",
                            100 * *progress / *progress_max
                        );
                        *progress_reported = 100 * *progress / *progress_max;
                    }

                    ix.start = pos;
                    ix.end = geompos_out.load(Ordering::Relaxed);
                    let indexpos = AtomicI64::new(0);
                    fwrite_check(&ix.to_bytes(), INDEX_SIZE, 1, indexfile, &indexpos, "index");
                }

                drop(indexmap);
                drop(geommap);
            } else {
                // We already reported the progress from splitting this radix out
                // but we need to split it again, which will be credited with more
                // progress. So increase the total amount of progress to report by
                // the additional progress that will happpen, which may move the
                // counter backward but will be an honest estimate of the work remaining.
                *progress_max += geomst_size / 4;

                let splits = (*availfiles / 4) as i32;
                radix1(
                    &geomfds[i..i + 1],
                    &indexfds[i..i + 1],
                    1,
                    prefix + splitbits,
                    splits,
                    mem,
                    tmpdir,
                    availfiles,
                    geomfile,
                    indexfile,
                    geompos_out,
                    progress,
                    progress_max,
                    progress_reported,
                    maxzoom,
                    basezoom,
                    droprate,
                    gamma,
                    ds,
                );
                already_closed = true;
            }
        }

        if !already_closed {
            if close_fd(geomfds[i]) < 0 {
                perror("close geom");
                std::process::exit(EXIT_CLOSE);
            }
            if close_fd(indexfds[i]) < 0 {
                perror("close index");
                std::process::exit(EXIT_CLOSE);
            }

            *availfiles += 2;
        }
    }
}

fn prep_drop_states(ds: &mut [DropState], maxzoom: i32, basezoom: i32, droprate: f64) {
    // Needs to be signed for interval calculation
    for i in 0..=(maxzoom as isize) {
        let d = &mut ds[i as usize];
        d.gap = 0.0;
        d.previndex = 0;
        d.interval = 0.0;

        if i < basezoom as isize {
            d.interval = (droprate.ln() * (basezoom as isize - i) as f64).exp();
        }

        d.seq = 0.0;
    }
}

#[allow(clippy::too_many_arguments)]
fn radix(
    readers: &[Reader],
    nreaders: i32,
    geomfile: &mut BufWriter<File>,
    indexfile: &mut BufWriter<File>,
    tmpdir: &str,
    geompos: &AtomicI64,
    maxzoom: i32,
    basezoom: i32,
    droprate: f64,
    gamma: f64,
) {
    // Run through the index and geometry for each reader,
    // splitting the contents out by index into as many
    // sub-files as we can write to simultaneously.

    // Then sort each of those by index, recursively if it is
    // too big to fit in memory.

    // Then concatenate each of the sub-outputs into a final output.

    let mut mem: i64 = get(&MEMSIZE) as i64;

    // Just for code coverage testing. Deeply recursive sorting is very slow
    // compared to sorting in memory.
    if additional(A_PREFER_RADIX_SORT) != 0 {
        mem = 8192;
    }

    let mut availfiles: i64 = MAX_FILES.load(Ordering::Relaxed)
        - 2 * nreaders as i64 // each reader has a geom and an index
        - 3 // pool, mbtiles, mbtiles journal
        - 4 // top-level geom and index output, both FILE and fd
        - 3; // stdin, stdout, stderr

    // 4 because for each we have output and input FILE and fd for geom and index
    let splits = (availfiles / 4) as i32;

    // Be somewhat conservative about memory availability because the whole point of this
    // is to keep from thrashing by working on chunks that will fit in memory.
    mem /= 2;

    let mut geom_total: i64 = 0;
    let mut geomfds: Vec<i32> = vec![0; nreaders as usize];
    let mut indexfds: Vec<i32> = vec![0; nreaders as usize];
    for i in 0..nreaders as usize {
        geomfds[i] = readers[i].geomfd;
        indexfds[i] = readers[i].indexfd;

        match fstat_size(readers[i].geomfd) {
            Ok(s) => geom_total += s,
            Err(e) => {
                set_errno(e);
                perror("stat geom");
                std::process::exit(EXIT_STAT);
            }
        }
    }

    let mut ds: Vec<DropState> = vec![DropState::default(); (maxzoom + 1) as usize];
    prep_drop_states(&mut ds, maxzoom, basezoom, droprate);

    let mut progress: i64 = 0;
    let mut progress_max: i64 = geom_total;
    let mut progress_reported: i64 = -1;
    let availfiles_before = availfiles;
    radix1(
        &geomfds,
        &indexfds,
        nreaders,
        0,
        splits,
        mem,
        tmpdir,
        &mut availfiles,
        geomfile,
        indexfile,
        geompos,
        &mut progress,
        &mut progress_max,
        &mut progress_reported,
        maxzoom,
        basezoom,
        droprate,
        gamma,
        &mut ds,
    );

    if availfiles - 2 * nreaders as i64 != availfiles_before {
        eprintln!(
            "Internal error: miscounted available file descriptors: {} vs {}",
            availfiles - 2 * nreaders as i64,
            availfiles
        );
        std::process::exit(EXIT_IMPOSSIBLE);
    }
}

#[allow(clippy::too_many_arguments)]
fn choose_first_zoom(
    file_bbox: &mut [i64; 4],
    file_bbox1: &mut [i64; 4],
    file_bbox2: &mut [i64; 4],
    readers: &mut [Reader],
    iz: &mut u32,
    ix: &mut u32,
    iy: &mut u32,
    minzoom: i32,
    buffer: i32,
) {
    for r in readers.iter_mut().take(get(&CPUS)) {
        let r = r.state.get_mut().unwrap();
        if r.file_bbox[0] < file_bbox[0] {
            file_bbox[0] = r.file_bbox[0];
        }
        if r.file_bbox[1] < file_bbox[1] {
            file_bbox[1] = r.file_bbox[1];
        }
        if r.file_bbox[2] > file_bbox[2] {
            file_bbox[2] = r.file_bbox[2];
        }
        if r.file_bbox[3] > file_bbox[3] {
            file_bbox[3] = r.file_bbox[3];
        }

        file_bbox1[0] = std::cmp::min(file_bbox1[0], r.file_bbox1[0]);
        file_bbox1[1] = std::cmp::min(file_bbox1[1], r.file_bbox1[1]);
        file_bbox1[2] = std::cmp::max(file_bbox1[2], r.file_bbox1[2]);
        file_bbox1[3] = std::cmp::max(file_bbox1[3], r.file_bbox1[3]);

        file_bbox2[0] = std::cmp::min(file_bbox2[0], r.file_bbox2[0]);
        file_bbox2[1] = std::cmp::min(file_bbox2[1], r.file_bbox2[1]);
        file_bbox2[2] = std::cmp::max(file_bbox2[2], r.file_bbox2[2]);
        file_bbox2[3] = std::cmp::max(file_bbox2[3], r.file_bbox2[3]);
    }

    // If the bounding box extends off the plane on either side,
    // a feature wrapped across the date line, so the width of the
    // bounding box is the whole world.
    if file_bbox[0] < 0 {
        file_bbox[0] = 0;
        file_bbox[2] = (1i64 << 32) - 1;
    }
    if file_bbox[2] > (1i64 << 32) - 1 {
        file_bbox[0] = 0;
        file_bbox[2] = (1i64 << 32) - 1;
    }
    if file_bbox[1] < 0 {
        file_bbox[1] = 0;
    }
    if file_bbox[3] > (1i64 << 32) - 1 {
        file_bbox[3] = (1i64 << 32) - 1;
    }

    let mut z = minzoom as isize;
    while z >= 0 {
        let shift: i64 = 1i64 << (32 - z);

        let left = (file_bbox[0] - buffer as i64 * shift / 256) / shift;
        let top = (file_bbox[1] - buffer as i64 * shift / 256) / shift;
        let right = (file_bbox[2] + buffer as i64 * shift / 256) / shift;
        let bottom = (file_bbox[3] + buffer as i64 * shift / 256) / shift;

        if left == right && top == bottom {
            *iz = z as u32;
            *ix = left as u32;
            *iy = top as u32;
            break;
        }
        z -= 1;
    }
}

pub fn vertexcmp(v1: &Vertex, v2: &Vertex) -> i32 {
    if v1.mid < v2.mid {
        return -1;
    }
    if v1.mid > v2.mid {
        return 1;
    }

    if v1.p1 < v2.p1 {
        return -1;
    }
    if v1.p1 > v2.p1 {
        return 1;
    }

    if v1.p2 < v2.p2 {
        return -1;
    }
    if v1.p2 > v2.p2 {
        return 1;
    }

    0
}

/// `vertexcmp` on the records of the temporary vertex files.
fn vertexcmp_bytes(a: &[u8], b: &[u8]) -> i32 {
    vertexcmp(&Vertex::from_bytes(a), &Vertex::from_bytes(b))
}

/// `nodecmp` on the records of the temporary node files.
fn nodecmp_bytes(a: &[u8], b: &[u8]) -> i32 {
    match crate::serial::nodecmp(&Node::from_bytes(a), &Node::from_bytes(b)) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

pub fn round_droprate(r: f64) -> f64 {
    (r * 100000.0).round() / 100000.0
}

/// The base zoom and drop rate guessing's `struct tile`.
#[derive(Clone, Copy, Debug, Default)]
struct GuessTile {
    x: u32,
    y: u32,
    count: i64,
    fullcount: i64,
    gap: f64,
    previndex: u64,
}

#[allow(clippy::too_many_arguments)]
fn read_input(
    sources: &mut [Source],
    fname: &str,
    maxzoom: i32,
    minzoom: i32,
    basezoom: i32,
    basezoom_marker_width: f64,
    outdb: &mut Option<Mutex<Connection>>,
    outdir: Option<&str>,
    exclude: &BTreeSet<String>,
    include: &BTreeSet<String>,
    exclude_all: i32,
    filter: Option<&JsonTree>,
    droprate: f64,
    buffer: i32,
    tmpdir: &str,
    gamma: f64,
    read_parallel: i32,
    forcetable: i32,
    attribution: Option<&str>,
    uses_gamma: bool,
    file_bbox: &mut [i64; 4],
    file_bbox1: &mut [i64; 4],
    file_bbox2: &mut [i64; 4],
    prefilter: Option<&str>,
    postfilter: Option<&str>,
    description: Option<&str>,
    guess_maxzoom: bool,
    guess_cluster_maxzoom: bool,
    attribute_types: &HashMap<String, i32>,
    pgm: &str,
    attribute_accum: &HashMap<String, AttributeOp>,
    attribute_descriptions: &BTreeMap<String, String>,
    commandline: &str,
    minimum_maxzoom: i32,
) -> (i32, Metadata) {
    let mut maxzoom = maxzoom;
    let mut basezoom = basezoom;
    let mut droprate = droprate;

    let mut ret = EXIT_SUCCESS;
    let cpus = get(&CPUS);
    let full_detail = FULL_DETAIL.load(Ordering::Relaxed);
    let low_detail = LOW_DETAIL.load(Ordering::Relaxed);
    let min_detail = MIN_DETAIL.load(Ordering::Relaxed);

    let mut readers: Vec<Reader> = Vec::with_capacity(cpus);
    for _ in 0..cpus {
        let mut r = Reader::default();

        let mut poolname = format!("{}/pool.XXXXXXXX", tmpdir);
        let mut treename = format!("{}/tree.XXXXXXXX", tmpdir);
        let mut geomname = format!("{}/geom.XXXXXXXX", tmpdir);
        let mut indexname = format!("{}/index.XXXXXXXX", tmpdir);
        let mut vertexname = format!("{}/vertex.XXXXXXXX", tmpdir);
        let mut nodename = format!("{}/node.XXXXXXXX", tmpdir);

        r.poolfd = mkstemp_cloexec(&mut poolname);
        if r.poolfd < 0 {
            perror(&poolname);
            std::process::exit(EXIT_OPEN);
        }
        r.treefd = mkstemp_cloexec(&mut treename);
        if r.treefd < 0 {
            perror(&treename);
            std::process::exit(EXIT_OPEN);
        }
        r.geomfd = mkstemp_cloexec(&mut geomname);
        if r.geomfd < 0 {
            perror(&geomname);
            std::process::exit(EXIT_OPEN);
        }
        r.indexfd = mkstemp_cloexec(&mut indexname);
        if r.indexfd < 0 {
            perror(&indexname);
            std::process::exit(EXIT_OPEN);
        }
        r.vertexfd = mkstemp_cloexec(&mut vertexname);
        if r.vertexfd < 0 {
            perror(&vertexname);
            std::process::exit(EXIT_OPEN);
        }
        r.nodefd = mkstemp_cloexec(&mut nodename);
        if r.nodefd < 0 {
            perror(&nodename);
            std::process::exit(EXIT_OPEN);
        }

        {
            let rs = r.state.get_mut().unwrap();

            rs.poolfile = memfile_open(r.poolfd);
            if rs.poolfile.is_none() {
                perror(&poolname);
                std::process::exit(EXIT_OPEN);
            }
            rs.treefile = memfile_open(r.treefd);
            if rs.treefile.is_none() {
                perror(&treename);
                std::process::exit(EXIT_OPEN);
            }
            match fopen_oflag(&geomname, "wb", libc::O_WRONLY | O_CLOEXEC) {
                Some(f) => rs.geomfile = Some(BufWriter::new(f)),
                None => {
                    perror(&geomname);
                    std::process::exit(EXIT_OPEN);
                }
            }
            match fopen_oflag(&indexname, "wb", libc::O_WRONLY | O_CLOEXEC) {
                Some(f) => rs.indexfile = Some(BufWriter::new(f)),
                None => {
                    perror(&indexname);
                    std::process::exit(EXIT_OPEN);
                }
            }
            match fopen_oflag(&vertexname, "w+b", libc::O_RDWR | O_CLOEXEC) {
                Some(f) => rs.vertexfile = Some(BufWriter::new(f)),
                None => {
                    perror(&format!("open vertexfile {}", vertexname));
                    std::process::exit(EXIT_OPEN);
                }
            }
            match fopen_oflag(&nodename, "w+b", libc::O_RDWR | O_CLOEXEC) {
                Some(f) => rs.nodefile = Some(BufWriter::new(f)),
                None => {
                    perror(&nodename);
                    std::process::exit(EXIT_OPEN);
                }
            }
        }
        r.geompos.store(0, Ordering::Relaxed);
        r.indexpos.store(0, Ordering::Relaxed);
        r.vertexpos.store(0, Ordering::Relaxed);
        r.nodepos.store(0, Ordering::Relaxed);

        unlink(&poolname);
        unlink(&treename);
        unlink(&geomname);
        unlink(&indexname);
        unlink(&vertexname);
        unlink(&nodename);

        {
            let rs = r.state.get_mut().unwrap();

            // To distinguish a null value
            {
                let p = Stringpool::default();
                let mut in_memory = false;
                memfile_write(rs.treefile.as_mut().unwrap(), &p.to_bytes(), &mut in_memory);
            }
            r.treefile_off
                .store(rs.treefile.as_ref().unwrap().off as i64, Ordering::Relaxed);

            rs.file_bbox[0] = u32::MAX as i64;
            rs.file_bbox[1] = u32::MAX as i64;
            rs.file_bbox[2] = 0;
            rs.file_bbox[3] = 0;
        }

        readers.push(r);
    }

    {
        match crate::platform::disk_free(readers[0].geomfd) {
            Err(e) => {
                set_errno(e);
                perror("Warning: fstatfs");
                eprintln!("Tippecanoe cannot check whether disk space will run out during tiling.");
                DISKFREE.store(i64::MAX, Ordering::Relaxed);
            }
            Ok(free) => DISKFREE.store(free, Ordering::Relaxed),
        }
    }

    let progress_seq = AtomicI64::new(0);

    // 2 * CPUS: One per reader thread, one per tiling thread
    let initialized: Vec<AtomicI32> = (0..2 * cpus).map(|_| AtomicI32::new(0)).collect();
    let initial_x: Vec<AtomicU32> = (0..2 * cpus).map(|_| AtomicU32::new(0)).collect();
    let initial_y: Vec<AtomicU32> = (0..2 * cpus).map(|_| AtomicU32::new(0)).collect();

    let nlayers = sources.len();
    for l in 0..nlayers {
        if sources[l].layer.is_empty() {
            let src: &str = if sources[l].file.is_empty() {
                fname
            } else {
                &sources[l].file
            };

            // Find the last component of the pathname
            let src = cstr(src).as_bytes();
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
            let mut out: Vec<u8> = Vec::new();
            for &c in &trunc {
                if c.is_ascii_alphabetic() || c.is_ascii_digit() || c == b'_' || (c & 0x80) != 0 {
                    out.push(c);
                }
            }

            sources[l].layer = String::from_utf8_lossy(&out).into_owned();
            if sources[l].layer.is_empty() || !check_utf8(&out).is_empty() {
                sources[l].layer = format!("unknown{}", l);
            }

            if get(&QUIET) == 0 {
                eprintln!(
                    "For layer {}, using name \"{}\"",
                    l as i32, sources[l].layer
                );
            }
        }
    }

    let mut layermap: BTreeMap<String, LayermapEntry> = BTreeMap::new();
    for (l, s) in sources.iter().enumerate() {
        let mut e = LayermapEntry::new(l);
        e.description = s.description.clone();
        layermap.entry(s.layer.clone()).or_insert(e);
    }

    let layermaps: Vec<Mutex<BTreeMap<String, LayermapEntry>>> =
        (0..cpus).map(|_| Mutex::new(layermap.clone())).collect();

    let mut overall_offset: i64 = 0;
    let dist = Mutex::new(DistSums::default());

    let files_open_before_reading = open_fd(get_null_device(), libc::O_RDONLY | O_CLOEXEC, 0);
    if files_open_before_reading < 0 {
        perror("open /dev/null");
        std::process::exit(EXIT_OPEN);
    }
    if close_fd(files_open_before_reading) != 0 {
        perror("close");
        std::process::exit(EXIT_CLOSE);
    }

    let shared = ReadShared {
        readers: &readers,
        progress_seq: &progress_seq,
        exclude,
        include,
        exclude_all,
        layermaps: &layermaps,
        initialized: &initialized,
        initial_x: &initial_x,
        initial_y: &initial_y,
        attribute_types,
    };
    let filters = prefilter.is_some() || postfilter.is_some();

    let nsources = sources.len();
    for source in 0..nsources {
        let reading: String;
        let fd: i32;

        if sources[source].file.is_empty() {
            reading = "standard input".to_string();
            fd = 0;
        } else {
            reading = sources[source].file.clone();
            fd = open_fd(&sources[source].file, libc::O_RDONLY, O_CLOEXEC);
            if fd < 0 {
                perror(&sources[source].file);
                continue;
            }
        }

        let layer: usize = match layermap.get(&sources[source].layer) {
            Some(a) => a.id,
            None => {
                eprint!(
                    "Internal error: couldn't find layer {}",
                    sources[source].layer
                );
                std::process::exit(EXIT_IMPOSSIBLE);
            }
        };

        // geobuf
        if sources[source].format == "fgb"
            || (sources[source].file.len() > 4 && sources[source].file.ends_with(".fgb"))
        {
            let st_size = match fstat_size(fd) {
                Ok(s) => s,
                Err(e) => {
                    set_errno(e);
                    perror("fstat");
                    perror(&sources[source].file);
                    std::process::exit(EXIT_STAT);
                }
            };

            let map = match mmap_fd(fd, st_size as usize) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!(
                        "{}: mmap: {}: {}",
                        av(),
                        reading,
                        strerror(e.raw_os_error().unwrap_or(libc::EINVAL))
                    );
                    std::process::exit(EXIT_MEMORY);
                }
            };

            let mut sst: Vec<SerializationState<'_>> = Vec::with_capacity(cpus);
            for i in 0..cpus {
                sst.push(shared.sst(
                    i,
                    &reading,
                    overall_offset,
                    guess_maxzoom,
                    maxzoom,
                    uses_gamma,
                    filters,
                    basezoom,
                ));
            }

            parse_flatgeobuf(&mut sst, &map, layer as i32, &sources[layer].layer);

            {
                let mut d = dist.lock().unwrap();
                for s in &sst {
                    d.dist_sum += s.dist_sum;
                    d.dist_count += s.dist_count;
                    d.area_sum = s.area_sum;
                }
            }

            drop(map);
            if close_fd(fd) != 0 {
                perror("close");
                std::process::exit(EXIT_CLOSE);
            }

            overall_offset = sst[0].layer_seq;
            checkdisk(&readers);
            continue;
        }

        if sources[source].format == "geobuf"
            || (sources[source].file.len() > 7 && sources[source].file.ends_with(".geobuf"))
        {
            let st_size = match fstat_size(fd) {
                Ok(s) => s,
                Err(e) => {
                    set_errno(e);
                    perror("fstat");
                    perror(&sources[source].file);
                    std::process::exit(EXIT_STAT);
                }
            };

            let map = match mmap_fd(fd, st_size as usize) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!(
                        "{}: mmap: {}: {}",
                        av(),
                        reading,
                        strerror(e.raw_os_error().unwrap_or(libc::EINVAL))
                    );
                    std::process::exit(EXIT_MEMORY);
                }
            };

            let mut sst: Vec<SerializationState<'_>> = Vec::with_capacity(cpus);
            for i in 0..cpus {
                sst.push(shared.sst(
                    i,
                    &reading,
                    overall_offset,
                    guess_maxzoom,
                    maxzoom,
                    uses_gamma,
                    filters,
                    basezoom,
                ));
            }

            parse_geobuf(&mut sst, &map, layer as i32, &sources[layer].layer);

            {
                let mut d = dist.lock().unwrap();
                for s in &sst {
                    d.dist_sum += s.dist_sum;
                    d.dist_count += s.dist_count;
                    d.area_sum += s.area_sum;
                }
            }

            drop(map);
            if close_fd(fd) != 0 {
                perror("close");
                std::process::exit(EXIT_CLOSE);
            }

            overall_offset = sst[0].layer_seq;
            checkdisk(&readers);
            continue;
        }

        if sources[source].format == "csv"
            || (sources[source].file.len() > 4 && sources[source].file.ends_with(".csv"))
        {
            // XXX factor out this duplicated setup
            let mut sst: Vec<SerializationState<'_>> = Vec::with_capacity(cpus);
            for i in 0..cpus {
                sst.push(shared.sst(
                    i,
                    &reading,
                    overall_offset,
                    guess_maxzoom,
                    maxzoom,
                    uses_gamma,
                    filters,
                    basezoom,
                ));
            }

            parse_geocsv(
                &mut sst,
                &sources[source].file,
                layer as i32,
                &sources[layer].layer,
            );

            if close_fd(fd) != 0 {
                perror("close");
                std::process::exit(EXIT_CLOSE);
            }

            overall_offset = sst[0].layer_seq;
            checkdisk(&readers);
            continue;
        }

        let mut map: Option<Mmap> = None;
        let mut map_len: i64 = 0;

        let mut read_parallel_this: i32 = if read_parallel != 0 { b'\n' as i32 } else { 0 };

        if !(sources[source].file.len() > 3 && sources[source].file.ends_with(".gz")) {
            if let Ok(st_size) = fstat_size(fd) {
                let off = crate::platform::lseek(fd, 0, libc::SEEK_CUR);
                if off >= 0 {
                    map_len = st_size - off;
                    // No error if MAP_FAILED because check is below
                    if map_len > 0 && off % get_page_size() == 0 {
                        // SAFETY: the input file is not changed while mapped.
                        if let Ok(m) = unsafe {
                            MmapOptions::new()
                                .offset(off as u64)
                                .len(map_len as usize)
                                .map_copy_read_only(&*borrow_fd(fd))
                        } {
                            madvise(&m, Advice::Random); // sequential, but from several pointers at once
                            map = Some(m);
                        }
                    }
                }
            }
        }

        if let Some(m) = &map {
            if map_len > 0 {
                if m[0] == 0x1E {
                    read_parallel_this = 0x1E;
                }

                if read_parallel_this == 0 {
                    // Not a GeoJSON text sequence, so unmap and read serially

                    map = None;
                }
            }
        }

        if let (Some(m), true) = (&map, read_parallel_this != 0) {
            do_read_parallel(
                m,
                map_len,
                overall_offset,
                &reading,
                &shared,
                basezoom,
                layer as i32,
                maxzoom,
                &sources[layer].layer,
                uses_gamma,
                read_parallel_this,
                &dist,
                guess_maxzoom,
                filters,
            );
            overall_offset += map_len;
            checkdisk(&readers);

            map = None;
            drop(map);

            if close_fd(fd) != 0 {
                perror("close input file");
                std::process::exit(EXIT_CLOSE);
            }
        } else {
            let mut fp = match streamfdopen(fd, "r", &sources[layer].file) {
                Some(fp) => fp,
                None => {
                    perror(&sources[layer].file);
                    if close_fd(fd) != 0 {
                        perror("close source file");
                        std::process::exit(EXIT_CLOSE);
                    }
                    continue;
                }
            };

            let c = fp.peekc();
            if c == 0x1E {
                read_parallel_this = 0x1E;
            }

            if read_parallel_this != 0 {
                // Serial reading of chunks that are then parsed in parallel

                let mut readname = format!("{}/read.XXXXXXXX", tmpdir);
                let mut readfd = mkstemp_cloexec(&mut readname);
                if readfd < 0 {
                    perror(&readname);
                    std::process::exit(EXIT_OPEN);
                }
                // SAFETY: the writer takes over the new descriptor.
                let mut readfp = Some(BufWriter::new(unsafe {
                    crate::platform::file_sharing_fd(readfd)
                }));
                unlink(&readname);

                let is_parsing = AtomicI32::new(0);
                let mut ahead: i64 = 0;
                let mut initial_offset: i64 = overall_offset;

                const READ_BUF: usize = 2000;
                const PARSE_MIN: i64 = 10000000;
                const PARSE_MAX: i64 = 1024 * 1024 * 1024;

                let mut buf = [0u8; READ_BUF];

                std::thread::scope(|scope| {
                    let mut parallel_parser = None;
                    let mut parser_created = false;

                    let rpa = |fd: i32,
                               fp: BufWriter<File>,
                               offset: i64,
                               len: i64|
                     -> ReadParallelArg<'_> {
                        ReadParallelArg {
                            fd,
                            fp: streamfpopen(fp),
                            offset,
                            len,
                            is_parsing: &is_parsing,
                            separator: read_parallel_this,
                            reading: &reading,
                            shared: &shared,
                            maxzoom,
                            basezoom,
                            source: layer as i32,
                            layername: sources[layer].layer.clone(),
                            uses_gamma: gamma != 0.0,
                            dist: &dist,
                            want_dist: guess_maxzoom,
                            filters,
                        }
                    };

                    let mut n;
                    loop {
                        n = fp.read(&mut buf);
                        if n == 0 {
                            break;
                        }

                        let readingpos = AtomicI64::new(0);
                        fwrite_check(
                            &buf[..n],
                            1,
                            n,
                            readfp.as_mut().unwrap(),
                            &readingpos,
                            &reading,
                        );
                        ahead += n as i64;

                        if buf[n - 1] as i32 == read_parallel_this && ahead > PARSE_MIN {
                            // Don't let the streaming reader get too far ahead of the parsers.
                            // If the buffered input gets huge, even if the parsers are still running,
                            // wait for the parser thread instead of continuing to stream input.

                            if is_parsing.load(Ordering::SeqCst) == 0 || ahead >= PARSE_MAX {
                                if parser_created {
                                    join_parser(&mut parallel_parser);
                                    parser_created = false;
                                }

                                let _ = readfp.as_mut().unwrap().flush();

                                readname = format!("{}/read.XXXXXXXX", tmpdir);
                                let next_fd = mkstemp_cloexec(&mut readname);
                                if next_fd < 0 {
                                    perror(&readname);
                                    std::process::exit(EXIT_OPEN);
                                }
                                // SAFETY: the writer takes over the new descriptor.
                                let next_fp = BufWriter::new(unsafe {
                                    crate::platform::file_sharing_fd(next_fd)
                                });

                                let this_fp = readfp.replace(next_fp).unwrap();
                                let this_fd = std::mem::replace(&mut readfd, next_fd);
                                start_parsing(
                                    scope,
                                    rpa(this_fd, this_fp, initial_offset, ahead),
                                    &mut parallel_parser,
                                    &mut parser_created,
                                );

                                initial_offset += ahead;
                                overall_offset += ahead;
                                checkdisk(&readers);
                                ahead = 0;

                                unlink(&readname);
                            }
                        }
                    }

                    if parser_created {
                        join_parser(&mut parallel_parser);
                        parser_created = false;
                    }

                    let _ = readfp.as_mut().unwrap().flush();

                    if ahead > 0 {
                        let this_fp = readfp.take().unwrap();
                        start_parsing(
                            scope,
                            rpa(readfd, this_fp, initial_offset, ahead),
                            &mut parallel_parser,
                            &mut parser_created,
                        );

                        if parser_created {
                            join_parser(&mut parallel_parser);
                            parser_created = false;
                        }

                        overall_offset += ahead;
                        checkdisk(&readers);
                    } else {
                        // The C++ never closes this last temporary file when
                        // nothing was left to parse, so neither does this.
                        std::mem::forget(readfp.take());
                    }
                    let _ = parser_created;
                });
            } else {
                // Plain serial reading

                let sources_layer = sources[layer].layer.clone();
                let mut sst = shared.sst(
                    0,
                    &reading,
                    overall_offset,
                    guess_maxzoom,
                    maxzoom,
                    uses_gamma,
                    filters,
                    basezoom,
                );
                {
                    let d = dist.lock().unwrap();
                    sst.dist_sum = d.dist_sum;
                    sst.dist_count = d.dist_count;
                    sst.area_sum = d.area_sum;
                }

                {
                    let mut jp = fp.json_begin();
                    parse_json(&mut sst, &mut jp, layer as i32, &sources_layer);
                }
                overall_offset = sst.layer_seq;

                {
                    let mut d = dist.lock().unwrap();
                    d.dist_sum = sst.dist_sum;
                    d.dist_count = sst.dist_count;
                    d.area_sum = sst.area_sum;
                }
                checkdisk(&readers);
            }

            if fp.fclose() != 0 {
                perror("fclose input");
                std::process::exit(EXIT_CLOSE);
            }
        }
    }

    let files_open_after_reading = open_fd(get_null_device(), libc::O_RDONLY | O_CLOEXEC, 0);
    if files_open_after_reading < 0 {
        perror("open /dev/null");
        std::process::exit(EXIT_OPEN);
    }
    if close_fd(files_open_after_reading) != 0 {
        perror("close");
        std::process::exit(EXIT_CLOSE);
    }

    if files_open_after_reading > files_open_before_reading {
        eprintln!(
            "Internal error: Files left open after reading input. ({} vs {})",
            files_open_before_reading, files_open_after_reading
        );
        ret = EXIT_IMPOSSIBLE;
    }

    if get(&QUIET) == 0 {
        eprint!("                              \r");
        //     (stderr, "Read 10000.00 million features\r", *progress_seq / 1000000.0);
    }

    let DistSums {
        dist_sum,
        dist_count,
        area_sum,
    } = *dist.lock().unwrap();

    let mut layermaps: Vec<BTreeMap<String, LayermapEntry>> = layermaps
        .into_iter()
        .map(|m| m.into_inner().unwrap())
        .collect();

    let mut vertexpos: i64 = 0;
    let nodepos = AtomicI64::new(0);

    for r in readers.iter_mut().take(cpus) {
        let rs = r.state.get_mut().unwrap();
        if let Err(e) = rs.geomfile.take().unwrap().into_inner() {
            perror_io("fclose geom", e.error());
            std::process::exit(EXIT_CLOSE);
        }
        if let Err(e) = rs.indexfile.take().unwrap().into_inner() {
            perror_io("fclose index", e.error());
            std::process::exit(EXIT_CLOSE);
        }
        memfile_close(rs.treefile.take().unwrap());

        match fstat_size(r.geomfd) {
            Ok(s) => rs.geomst_size = s,
            Err(e) => {
                set_errno(e);
                perror("stat geom\n");
                std::process::exit(EXIT_STAT);
            }
        }

        vertexpos += r.vertexpos.load(Ordering::Relaxed);
        nodepos.fetch_add(r.nodepos.load(Ordering::Relaxed), Ordering::Relaxed);
    }

    if get(&QUIET) == 0 {
        eprint!("Merging string pool           \r");
    }

    // Create a combined string pool
    // but keep track of the offsets into it since we still need
    // segment+offset to find the data.

    // 2 * CPUS: One per input thread, one per tiling thread
    let mut pool_off: Vec<i64> = vec![0; 2 * cpus];

    let mut poolname = format!("{}/pool.XXXXXXXX", tmpdir);

    let poolfd = mkstemp_cloexec(&mut poolname);
    if poolfd < 0 {
        perror(&poolname);
        std::process::exit(EXIT_OPEN);
    }

    let mut poolfile = match fopen_oflag(&poolname, "wb", libc::O_WRONLY | O_CLOEXEC) {
        Some(f) => BufWriter::new(f),
        None => {
            perror(&poolname);
            std::process::exit(EXIT_OPEN);
        }
    };

    unlink(&poolname);
    let mut poolpos: i64 = 0;

    for (i, r) in readers.iter_mut().enumerate().take(cpus) {
        // If the memfile is not done yet, it is in memory, so just copy the memory.
        // Otherwise, we need to merge memory and file.

        let rs = r.state.get_mut().unwrap();
        let mut pf = rs.poolfile.take().unwrap();
        if pf.fp.is_none() {
            // still in memory

            if !pf.map.is_empty() && !fwrite_ok(&mut poolfile, &pf.map) {
                perror("Reunify string pool");
                std::process::exit(EXIT_WRITE);
            }

            pool_off[i] = poolpos;
            poolpos += pf.map.len() as i64;
        } else {
            // split into memory and file

            if pf.fp.as_mut().unwrap().flush().is_err() {
                perror("fflush poolfile");
                std::process::exit(EXIT_WRITE);
            }

            let s = match mmap_fd(pf.fd, pf.off) {
                Ok(m) => m,
                Err(e) => {
                    perror_io("mmap string pool for copy", &e);
                    std::process::exit(EXIT_MEMORY);
                }
            };
            madvise(&s, Advice::Sequential);
            if !fwrite_ok(&mut poolfile, &s[..pf.off]) {
                perror("Reunify string pool (split)");
                std::process::exit(EXIT_WRITE);
            }
            drop(s);

            pool_off[i] = poolpos;
            poolpos += pf.off as i64;
        }

        memfile_close(pf);
    }

    if let Err(e) = poolfile.into_inner() {
        perror_io("fclose pool", e.error());
        std::process::exit(EXIT_CLOSE);
    }

    let mut stringpool = StringPoolRef::default();
    if poolpos > 0 {
        // Will be 0 if -X was specified
        match mmap_fd(poolfd, poolpos as usize) {
            Ok(m) => {
                madvise(&m, Advice::Random);
                let pool: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(m);
                stringpool.pool = Some(pool);
            }
            Err(e) => {
                perror_io("mmap string pool", &e);
                std::process::exit(EXIT_MEMORY);
            }
        }
    }

    if get(&QUIET) == 0 {
        eprint!("Merging vertices              \r");
    }

    // Sort the vertices;
    // find nodes where the same central point is part of two different vertices
    {
        let mut tmpname = format!("{}/vertex2.XXXXXX", tmpdir);
        let vertexfd = mkstemp(&mut tmpname);
        if vertexfd < 0 {
            perror(&format!("mkstemp vertexfile {}", tmpname));
            std::process::exit(EXIT_OPEN);
        }
        unlink(&tmpname);
        // SAFETY: the writer takes over the new descriptor.
        let mut vertex_out = BufWriter::new(unsafe { crate::platform::file_from_fd(vertexfd) });

        let mut vertex_readers: Vec<BufReader<File>> = Vec::new();
        for r in readers.iter_mut().take(cpus) {
            let rs = r.state.get_mut().unwrap();
            vertex_readers.push(rewind(rs.vertexfile.take().unwrap()));
        }
        {
            let mut refs: Vec<&mut dyn Read> = vertex_readers
                .iter_mut()
                .map(|r| r as &mut dyn Read)
                .collect();
            fqsort(
                &mut refs,
                VERTEX_SIZE,
                vertexcmp_bytes,
                &mut vertex_out,
                get(&MEMSIZE) / 20,
                tmpdir,
            );
        }

        drop(vertex_readers);

        let mut vertex_in = rewind(vertex_out);

        let zero = Draw::new(VT_MOVETO, 0, 0);
        let mut prev = Vertex::new(zero, zero, zero);
        let mut vbuf = [0u8; VERTEX_SIZE];
        let geometry_scale = get(&GEOMETRY_SCALE);
        let r0 = &mut readers[0];
        let nodefile0 = r0.state.get_mut().unwrap().nodefile.as_mut().unwrap();
        while fread_record(&mut vertex_in, &mut vbuf) {
            let v = Vertex::from_bytes(&vbuf);
            if v.mid == prev.mid && (v.p1 != prev.p1 || v.p2 != prev.p2) {
                let x = v.mid.x * (1i64 << geometry_scale);
                let y = v.mid.y * (1i64 << geometry_scale);

                let n = Node {
                    index: encode_vertex(x as u32, y as u32),
                };

                fwrite_check(
                    &n.to_bytes(),
                    NODE_SIZE,
                    1,
                    nodefile0,
                    &r0.nodepos,
                    "vertices",
                );
            }
            prev = v;
        }

        drop(vertex_in);
    }

    if get(&QUIET) == 0 {
        eprint!("Merging nodes                 \r");
    }

    let mut shared_nodes_bloom: Vec<u8> = vec![0; 34567891]; // circa 34MB, size nowhere near a power of 2

    // Sort nodes that can't be simplified away; scan the list to remove duplicates

    let shared_nodes: BufWriter<File>;
    let mut shared_nodes_map: Option<Mmap> = None; // will be null if there are no shared nodes
    {
        // sort

        let mut tmpname = format!("{}/node2.XXXXXX", tmpdir);
        let nodefd = mkstemp(&mut tmpname);
        if nodefd < 0 {
            perror(&format!("mkstemp nodefile {}", tmpname));
            std::process::exit(EXIT_OPEN);
        }
        unlink(&tmpname);
        // SAFETY: the writer takes over the new descriptor.
        let mut node_out = BufWriter::new(unsafe { crate::platform::file_from_fd(nodefd) });

        let mut node_readers: Vec<BufReader<File>> = Vec::new();
        for r in readers.iter_mut().take(cpus) {
            let rs = r.state.get_mut().unwrap();
            node_readers.push(rewind(rs.nodefile.take().unwrap()));
        }

        {
            let mut refs: Vec<&mut dyn Read> = node_readers
                .iter_mut()
                .map(|r| r as &mut dyn Read)
                .collect();
            fqsort(
                &mut refs,
                NODE_SIZE,
                nodecmp_bytes,
                &mut node_out,
                get(&MEMSIZE) / 20,
                tmpdir,
            );
        }

        drop(node_readers);

        let mut node_in = rewind(node_out);

        // scan

        tmpname = format!("{}/node3.XXXXXX", tmpdir);
        let nodefd = mkstemp(&mut tmpname);
        if nodefd < 0 {
            perror(&format!("mkstemp nodefile {}", tmpname));
            std::process::exit(EXIT_OPEN);
        }
        unlink(&tmpname);
        // SAFETY: the writer takes over the new descriptor.
        let mut sn = BufWriter::new(unsafe { crate::platform::file_sharing_fd(nodefd) });

        // `written` is to see if this node has already been preserved
        // and doesn't need to be preserved again
        let mut written = Node { index: u64::MAX };

        nodepos.store(0, Ordering::Relaxed);
        let mut nbuf = [0u8; NODE_SIZE];
        let bloom_bits = (shared_nodes_bloom.len() * 8) as u64;
        while fread_record(&mut node_in, &mut nbuf) {
            let here = Node::from_bytes(&nbuf);
            if crate::serial::nodecmp(&here, &written) != std::cmp::Ordering::Equal {
                fwrite_check(
                    &here.to_bytes(),
                    NODE_SIZE,
                    1,
                    &mut sn,
                    &nodepos,
                    "shared nodes",
                );
                written = here;

                let mut bloom_ix = (here.index % bloom_bits) as usize;
                let bloom_mask: u8 = 1 << (bloom_ix & 7);
                bloom_ix >>= 3;
                shared_nodes_bloom[bloom_ix] |= bloom_mask;
            }
        }

        let _ = sn.flush();

        let np = nodepos.load(Ordering::Relaxed);
        if np > 0 {
            match mmap_fd(nodefd, np as usize) {
                Ok(m) => shared_nodes_map = Some(m),
                Err(e) => {
                    perror_io("mmap nodes", &e);
                    std::process::exit(EXIT_MEMORY);
                }
            }
        }
        crate::platform::close_shared_fd(nodefd);

        shared_nodes = sn;
        drop(node_in);
    }
    let nodepos: i64 = nodepos.load(Ordering::Relaxed);

    if get(&QUIET) == 0 {
        eprint!("Merging index                 \r");
    }

    let mut indexname = format!("{}/index.XXXXXXXX", tmpdir);
    let (indexfd, mut indexfile) = temp_pair(&mut indexname, EXIT_OPEN);
    unlink(&indexname);

    let mut geomname = format!("{}/geom.XXXXXXXX", tmpdir);
    let (geomfd, mut geomfile) = temp_pair(&mut geomname, EXIT_CLOSE);
    unlink(&geomname);

    let mut iz: u32 = 0;
    let mut ix: u32 = 0;
    let mut iy: u32 = 0;
    choose_first_zoom(
        file_bbox,
        file_bbox1,
        file_bbox2,
        &mut readers,
        &mut iz,
        &mut ix,
        &mut iy,
        minzoom,
        buffer,
    );

    if JUSTX.load(Ordering::Relaxed) >= 0 {
        iz = minzoom as u32;
        ix = JUSTX.load(Ordering::Relaxed) as u32;
        iy = JUSTY.load(Ordering::Relaxed) as u32;
    }

    let geompos = AtomicI64::new(0);

    /* initial tile is normally 0/0/0 but can be iz/ix/iy if limited to one tile */
    let mut estimated_complexity: i64 = 0; // to be replaced after writing the data
    fwrite_check(
        &estimated_complexity.to_ne_bytes(),
        8,
        1,
        &mut geomfile,
        &geompos,
        fname,
    );
    fserialize_int(&mut geomfile, iz as i32, &geompos, fname);
    fserialize_uint(&mut geomfile, ix, &geompos, fname);
    fserialize_uint(&mut geomfile, iy, &geompos, fname);

    radix(
        &readers,
        cpus as i32,
        &mut geomfile,
        &mut indexfile,
        tmpdir,
        &geompos,
        maxzoom,
        basezoom,
        droprate,
        gamma,
    );

    /* end of tile */
    fserialize_ulong_long(&mut geomfile, 0, &geompos, fname); // EOF

    estimated_complexity = geompos.load(Ordering::Relaxed);
    let _ = geomfile.flush();
    match geomfile
        .get_ref()
        .write_at(&estimated_complexity.to_ne_bytes(), 0)
    {
        Ok(8) => {}
        Ok(_) => {
            perror("pwrite estimated complexity");
            std::process::exit(EXIT_WRITE);
        }
        Err(e) => {
            perror_io("pwrite estimated complexity", &e);
            std::process::exit(EXIT_WRITE);
        }
    }

    if let Err(e) = geomfile.into_inner() {
        perror_io("fclose geom", e.error());
        std::process::exit(EXIT_CLOSE);
    }
    if let Err(e) = indexfile.into_inner() {
        perror_io("fclose index", e.error());
        std::process::exit(EXIT_CLOSE);
    }

    let indexst_size = match fstat_size(indexfd) {
        Ok(s) => s,
        Err(e) => {
            set_errno(e);
            perror("stat index");
            std::process::exit(EXIT_STAT);
        }
    };
    let indexpos: i64 = indexst_size;
    progress_seq.store(indexpos / INDEX_SIZE as i64, Ordering::Relaxed);

    LAST_PROGRESS.store(0.0);
    if get(&QUIET) == 0 {
        let s = progress_seq.load(Ordering::Relaxed);
        let geompos_print = geompos.load(Ordering::Relaxed);
        let poolpos_print = poolpos;
        let vertexpos_print = vertexpos;
        let nodepos_print = nodepos;
        eprintln!(
            "{} features, {} bytes of geometry and attributes, {} bytes of string pool, {} bytes of vertices, {} bytes of nodes",
            s, geompos_print, poolpos_print, vertexpos_print, nodepos_print
        );
    }

    if indexpos == 0 {
        eprintln!("Did not read any valid geometries");
        if let Some(db) = outdb.take() {
            mbtiles_close(db.into_inner().unwrap(), pgm);
        }
        std::process::exit(EXIT_NODATA);
    }

    let map = match mmap_fd(indexfd, indexpos as usize) {
        Ok(m) => m,
        Err(e) => {
            perror_io("mmap index for basezoom", &e);
            std::process::exit(EXIT_MEMORY);
        }
    };
    madvise(&map, Advice::Sequential);
    madvise(&map, Advice::WillNeed);
    let indices: i64 = indexpos / INDEX_SIZE as i64;
    let map_at = |ip: i64| Index::from_bytes(&map[ip as usize * INDEX_SIZE..]);
    let mut fix_dropping = false;

    if guess_maxzoom {
        let mut mean: f64 = 0.0;
        let mut count: usize = 0;
        let mut m2: f64 = 0.0;
        let mut dupes: usize = 0;

        let mut progress: i64 = -1;
        for ip in 1..indices {
            let here = map_at(ip);
            let before = map_at(ip - 1);
            if here.ix != before.ix {
                // https://en.wikipedia.org/wiki/Algorithms_for_calculating_variance#Welford's_online_algorithm
                let new_value = ((here.ix - before.ix) as f64).ln();
                count += 1;
                let delta = new_value - mean;
                mean += delta / count as f64;
                let delta2 = new_value - mean;
                m2 += delta * delta2;
            } else {
                dupes += 1;
            }

            let nprogress = 100 * ip / indices;
            if nprogress != progress {
                progress = nprogress;
                if get(&QUIET) == 0 && get(&QUIET_PROGRESS) == 0 && progress_time() {
                    eprint!("Maxzoom: {}% \r", progress);
                }
            }
        }

        if count == 0 && dist_count == 0 && minimum_maxzoom == 0 {
            eprintln!("Can't guess maxzoom (-zg) without at least two distinct feature locations");
            if let Some(db) = outdb.take() {
                mbtiles_close(db.into_inner().unwrap(), pgm);
            }
            std::process::exit(EXIT_NODATA);
        }

        if count == 0 && dist_count == 0 {
            maxzoom = minimum_maxzoom;
            if droprate < 0.0 {
                droprate = 1.0;
            }
        } else if count > 0 {
            let stddev = (m2 / count as f64).sqrt();

            // Geometric mean is appropriate because distances between features
            // are typically lognormally distributed. Two standard deviations
            // below the mean should be enough to distinguish most features.
            let avg = mean.exp();
            let nearby = (mean - 1.5 * stddev).exp();

            // Convert approximately from tile units to feet.
            // See empirical data above for source
            let dist_ft = avg.sqrt() / 33.0;
            let nearby_ft = nearby.sqrt() / 33.0;

            // Go one zoom level beyond what is strictly necessary for nearby features.
            let want = nearby_ft / 2.0;

            maxzoom =
                ((360.0 / (0.00000274 * want)).ln() / 2f64.ln() - full_detail as f64).ceil() as i32;
            if maxzoom < 0 {
                maxzoom = 0;
            }
            if get(&QUIET) == 0 {
                eprint!(
                    "Choosing a maxzoom of -z{} for features typically {} feet ({} meters) apart, ",
                    maxzoom,
                    dist_ft.ceil() as i32,
                    (dist_ft / 3.28084).ceil() as i32
                );
                eprintln!(
                    "and at least {} feet ({} meters) apart",
                    nearby_ft.ceil() as i32,
                    (nearby_ft / 3.28084).ceil() as i32
                );
            }

            let cluster_maxzoom = get(&CLUSTER_MAXZOOM);
            let cluster_distance = get(&CLUSTER_DISTANCE);
            let mut changed = false;
            while maxzoom < 32 - full_detail
                && maxzoom < 33 - low_detail
                && maxzoom < cluster_maxzoom
                && cluster_distance > 0
            {
                let a = (1i64 << (32 - maxzoom)) / 256 * cluster_distance as i64;
                let zoom_mingap: u64 = a.wrapping_mul(a) as u64;
                if avg > zoom_mingap as f64 {
                    break;
                }

                maxzoom += 1;
                changed = true;
            }
            if changed {
                println!(
                    "Choosing a maxzoom of -z{} to keep most features distinct with cluster distance {} and cluster maxzoom {}",
                    maxzoom, cluster_distance, cluster_maxzoom
                );
            }

            if droprate == -3.0 {
                // This mysterious formula is the result of eyeballing the appropriate drop rate
                // for several point tilesets using -zg and then fitting a curve to the pattern
                // that emerged. It appears that if the standard deviation of the distances between
                // features is small, the drop rate should be large because the features are evenly
                // spaced, and if the standard deviation is large, the drop rate can be small because
                // the features are in clumps.
                droprate = round_droprate((-0.7681 * stddev.ln() + 1.582).exp());

                if droprate < 0.0 {
                    droprate = 0.0;
                }

                if get(&QUIET) == 0 {
                    eprintln!("Choosing a drop rate of {:.6}", droprate);
                }

                if dupes != 0 && droprate != 0.0 {
                    maxzoom = (maxzoom as f64
                        + ((((dupes + count) / count) as f64).ln() / droprate.ln()).round())
                        as i32;
                    if get(&QUIET) == 0 {
                        eprintln!(
                            "Increasing maxzoom to {} to account for {} duplicate feature locations",
                            maxzoom, dupes
                        );
                    }
                }
            }
        }

        if dist_count != 0 {
            // no conversion to pseudo-feet here because that already happened within each feature
            let want2 = (dist_sum / dist_count as f64).exp() / 8.0;
            let mz = ((360.0 / (0.00000274 * want2)).ln() / 2f64.ln() - full_detail as f64).ceil()
                as i32;

            if mz > maxzoom || count == 0 {
                if get(&QUIET) == 0 {
                    eprintln!(
                        "Choosing a maxzoom of -z{} for resolution of about {} feet ({} meters) within features",
                        mz,
                        (dist_sum / dist_count as f64).exp() as i32,
                        ((dist_sum / dist_count as f64).exp() / 3.28084) as i32
                    );
                }
                maxzoom = mz;
            }
        }

        if maxzoom < 0 {
            maxzoom = 0;
        }
        if maxzoom > 32 - full_detail {
            maxzoom = 32 - full_detail;
        }
        if maxzoom > 33 - low_detail {
            // that is, maxzoom - 1 > 32 - low_detail
            maxzoom = 33 - low_detail;
        }

        let mut total_tile_count: f64 = 0.0;
        for i in 1..=maxzoom {
            let tile_count = (area_sum / (((1i64 << (32 - i)) * (1i64 << (32 - i))) as f64)).ceil();
            total_tile_count += tile_count;

            // 2M tiles is an arbitrary limit, chosen to make tiling jobs
            // that seem like they should finish in a few minutes
            // actually finish in a few minutes. It is large enough to
            // tile a polygon that covers the entire world to z10
            // or the United States to z13.

            if total_tile_count > (2 * 1024 * 1024) as f64 {
                println!(
                    "Limiting maxzoom to -z{} to keep from generating {} tiles",
                    i - 1,
                    total_tile_count as i64
                );
                maxzoom = i - 1;
                break;
            }
        }

        if basezoom == -2 && basezoom_marker_width == 1.0 {
            // -Bg, not -Bg###
            basezoom = maxzoom;
            if get(&QUIET) == 0 {
                eprintln!("Using base zoom of -z{}", basezoom);
            }
        }

        if maxzoom < minimum_maxzoom {
            if get(&QUIET) == 0 {
                eprintln!("Using minimum maxzoom of -z{}", minimum_maxzoom);
            }
            maxzoom = minimum_maxzoom;
        }

        if maxzoom < minzoom {
            if get(&QUIET) == 0 {
                eprintln!(
                    "Can't use {} for maxzoom because minzoom is {}",
                    maxzoom, minzoom
                );
            }
            maxzoom = minzoom;
        }

        fix_dropping = true;

        if basezoom == -1 {
            // basezoom unspecified
            basezoom = maxzoom;
        }
    }

    if get(&CLUSTER_MAXZOOM) >= maxzoom && guess_cluster_maxzoom {
        CLUSTER_MAXZOOM.store(maxzoom - 1, Ordering::Relaxed);
        eprintln!(
            "Choosing a cluster maxzoom of -k{} to make all features visible at maximum zoom {}",
            get(&CLUSTER_MAXZOOM),
            maxzoom
        );
    }

    if basezoom < 0 || droprate < 0.0 {
        let mut tile = [GuessTile::default(); MAX_ZOOM as usize + 1];
        let mut max = [GuessTile::default(); MAX_ZOOM as usize + 1];

        {
            for z in 0..=MAX_ZOOM as usize {
                tile[z].x = 0;
                tile[z].y = 0;
                tile[z].count = 0;
                tile[z].fullcount = 0;
                tile[z].gap = 0.0;
                tile[z].previndex = 0;
                max[z].x = 0;
                max[z].y = 0;
                max[z].count = 0;
                max[z].fullcount = 0;
            }
        }

        let mut progress: i64 = -1;

        for ip in 0..indices {
            let here = map_at(ip);
            let (xx, yy) = decode_index(here.ix);

            let nprogress = 100 * ip / indices;
            if nprogress != progress {
                progress = nprogress;
                if get(&QUIET) == 0 && get(&QUIET_PROGRESS) == 0 && progress_time() {
                    eprint!("Base zoom/drop rate: {}% \r", progress);
                }
            }

            for z in 0..=MAX_ZOOM {
                let zu = z as usize;
                let mut xxx: u32 = 0;
                let mut yyy: u32 = 0;
                if z != 0 {
                    // These are tile numbers, not pixels,
                    // so shift, not round
                    xxx = xx >> (32 - z);
                    yyy = yy >> (32 - z);
                }

                let scale = (1i64 << (64 - 2 * (z + 8))) as f64;

                if tile[zu].x != xxx || tile[zu].y != yyy {
                    if tile[zu].count > max[zu].count {
                        max[zu] = tile[zu];
                    }

                    tile[zu].x = xxx;
                    tile[zu].y = yyy;
                    tile[zu].count = 0;
                    tile[zu].fullcount = 0;
                    tile[zu].gap = 0.0;
                    tile[zu].previndex = 0;
                }

                tile[zu].fullcount += 1;

                if manage_gap(
                    here.ix,
                    &mut tile[zu].previndex,
                    scale,
                    gamma,
                    &mut tile[zu].gap,
                ) != 0
                {
                    continue;
                }

                tile[zu].count += 1;
            }
        }

        let mut z = MAX_ZOOM as isize;
        while z >= 0 {
            if tile[z as usize].count > max[z as usize].count {
                max[z as usize] = tile[z as usize];
            }
            z -= 1;
        }

        let max_features = (50000.0 / (basezoom_marker_width * basezoom_marker_width)) as i32;

        let obasezoom = basezoom;
        if basezoom < 0 {
            basezoom = MAX_ZOOM;

            let mut z = MAX_ZOOM;
            while z >= 0 {
                if max[z as usize].count < max_features as i64 {
                    basezoom = z;
                }

                // printf("%d/%u/%u %lld\n", z, max[z].x, max[z].y, max[z].count);
                z -= 1;
            }

            if get(&QUIET) == 0 {
                let b = basezoom as usize;
                eprintln!(
                    "Choosing a base zoom of -B{} to keep {} features in tile {}/{}/{}.",
                    basezoom, max[b].count, basezoom, max[b].x, max[b].y
                );
            }
        }

        if obasezoom < 0 && basezoom > maxzoom && prevent(P_BASEZOOM_ABOVE_MAXZOOM) != 0 {
            basezoom = maxzoom;
        }

        if obasezoom < 0 && basezoom > maxzoom {
            eprintln!("Couldn't find a suitable base zoom. Working from the other direction.");
            if gamma == 0.0 {
                eprintln!("You might want to try -g1 to limit near-duplicates.");
            }

            let mz = maxzoom as usize;
            if droprate < 0.0 {
                if maxzoom == 0 {
                    droprate = 2.5;
                } else {
                    droprate = round_droprate(
                        ((max[0].count as f64 / max[mz].count as f64).ln() / maxzoom as f64).exp(),
                    );
                    if get(&QUIET) == 0 {
                        eprintln!(
                            "Choosing a drop rate of -r{:.6} to get from {} to {} in {} zooms",
                            droprate, max[mz].count, max[0].count, maxzoom
                        );
                    }
                }
            }

            basezoom = 0;
            for z in 0..=maxzoom {
                let zoomdiff =
                    (max[z as usize].count as f64 / max_features as f64).ln() / droprate.ln();
                if zoomdiff + z as f64 > basezoom as f64 {
                    basezoom = (zoomdiff + z as f64).ceil() as i32;
                }
            }

            if get(&QUIET) == 0 {
                eprintln!(
                    "Choosing a base zoom of -B{} to keep {:.6} features in tile {}/{}/{}.",
                    basezoom,
                    max[mz].count as f64 * (droprate.ln() * (maxzoom - basezoom) as f64).exp(),
                    maxzoom,
                    max[mz].x,
                    max[mz].y
                );
            }
        } else if droprate < 0.0 {
            droprate = 1.0;

            let mut z = basezoom - 1;
            while z >= 0 {
                let zu = z as usize;
                let mut interval = (droprate.ln() * (basezoom - z) as f64).exp();

                if max[zu].count as f64 / interval >= max_features as f64 {
                    interval = max[zu].count as f64 / max_features as f64;
                    droprate = round_droprate((interval.ln() / (basezoom - z) as f64).exp());
                    interval = (droprate.ln() * (basezoom - z) as f64).exp();

                    if get(&QUIET) == 0 {
                        eprintln!(
                            "Choosing a drop rate of -r{:.6} to keep {:.6} features in tile {}/{}/{}.",
                            droprate,
                            max[zu].count as f64 / interval,
                            z,
                            max[zu].x,
                            max[zu].y
                        );
                    }
                }
                z -= 1;
            }
        }

        if gamma > 0.0 {
            let mut effective: i32 = 0;

            for z in 0..maxzoom {
                if max[z as usize].count < max[z as usize].fullcount {
                    effective = z + 1;
                }
            }

            if effective == 0 {
                if get(&QUIET) == 0 {
                    eprintln!("With gamma, effective base zoom is 0, so no effective drop rate");
                }
            } else {
                let e = effective as usize;
                let interval_0 = (droprate.ln() * (basezoom - 0) as f64).exp();
                let mut interval_eff = (droprate.ln() * (basezoom - effective) as f64).exp();
                if effective > basezoom {
                    interval_eff = 1.0;
                }

                let scaled_0 = max[0].count as f64 / interval_0;
                let scaled_eff = max[e].count as f64 / interval_eff;

                let rate_at_0 = scaled_0 / max[0].fullcount as f64;
                let rate_at_eff = scaled_eff / max[e].fullcount as f64;

                let eff_drop = ((rate_at_eff / rate_at_0).ln() / (effective - 0) as f64).exp();

                if get(&QUIET) == 0 {
                    eprintln!(
                        "With gamma, effective base zoom of {}, effective drop rate of {:.6}",
                        effective, eff_drop
                    );
                }
            }
        }

        fix_dropping = true;
    }

    let drop_denser = DROP_DENSER.load(Ordering::Relaxed);
    if fix_dropping || drop_denser > 0 {
        // Fix up the minzooms for features, now that we really know the base zoom
        // and drop rate.

        let geomst_size = match fstat_size(geomfd) {
            Ok(s) => s,
            Err(e) => {
                set_errno(e);
                perror("stat sorted geom\n");
                std::process::exit(EXIT_STAT);
            }
        };
        // SAFETY: the sorted geometry file is changed only through this map
        // while it is mapped.
        let mut geom: MmapMut = match unsafe {
            MmapOptions::new()
                .len(geomst_size as usize)
                .map_mut(&*borrow_fd(geomfd))
        } {
            Ok(m) if geomst_size > 0 => m,
            Ok(_) => {
                set_errno(libc::EINVAL);
                perror("mmap geom for fixup");
                std::process::exit(EXIT_MEMORY);
            }
            Err(e) => {
                perror_io("mmap geom for fixup", &e);
                std::process::exit(EXIT_MEMORY);
            }
        };
        crate::platform::madvise_mut(&geom, Advice::Sequential);
        crate::platform::madvise_mut(&geom, Advice::WillNeed);

        let mut ds: Vec<DropState> = vec![DropState::default(); (maxzoom + 1) as usize];
        prep_drop_states(&mut ds, maxzoom, basezoom, droprate);

        if drop_denser > 0 {
            let mut ddv: Vec<DropDensest> = Vec::new();
            let mut previndex: u64 = 0;

            for ip in 0..indices {
                let here = map_at(ip);
                let t = here.t() as i32;
                if t == VT_POINT
                    || (additional(A_LINE_DROP) != 0 && t == VT_LINE)
                    || (additional(A_POLYGON_DROP) != 0 && t == VT_POLYGON)
                {
                    if here.ix % 100 < drop_denser as u64 {
                        let dd = DropDensest {
                            gap: here.ix.wrapping_sub(previndex),
                            seq: ip as usize,
                        };
                        ddv.push(dd);

                        previndex = here.ix;
                    } else {
                        let feature_minzoom = calc_feature_minzoom(&here, &mut ds, maxzoom, gamma);
                        geom[(here.end - 1) as usize] = feature_minzoom as u8;
                    }
                }
            }

            // largest gap sorts first
            ddv.sort_by(|a, b| b.gap.cmp(&a.gap));

            let mut i: usize = 0;
            for z in 0..=basezoom {
                let keep_fraction = 1.0 / (droprate.ln() * (basezoom - z) as f64).exp();
                let keep_count = (ddv.len() as f64 * keep_fraction) as usize;

                while i < keep_count && i < ddv.len() {
                    geom[(map_at(ddv[i].seq as i64).end - 1) as usize] = z as u8;
                    i += 1;
                }
            }
            while i < ddv.len() {
                geom[(map_at(ddv[i].seq as i64).end - 1) as usize] = basezoom as u8;
                i += 1;
            }
        } else {
            for ip in 0..indices {
                let here = map_at(ip);
                if ip > 0 && here.start != map_at(ip - 1).end {
                    eprintln!("Mismatched index at {}: {} vs {}", ip, here.start, here.end);
                }
                let feature_minzoom = calc_feature_minzoom(&here, &mut ds, maxzoom, gamma);
                geom[(here.end - 1) as usize] = feature_minzoom as u8;
            }
        }

        drop(geom);
    }

    drop(map);

    if close_fd(indexfd) != 0 {
        perror("close sorted index");
    }

    /* Traverse and split the geometries for each zoom level */

    let geomst_size = match fstat_size(geomfd) {
        Ok(s) => s,
        Err(e) => {
            set_errno(e);
            perror("stat sorted geom\n");
            std::process::exit(EXIT_STAT);
        }
    };

    let temp_files = get(&TEMP_FILES);
    let mut fd: Vec<i32> = vec![0; temp_files];
    let mut size: Vec<i64> = vec![0; temp_files];

    fd[0] = geomfd;
    size[0] = geomst_size;

    for j in 1..temp_files {
        fd[j] = -1;
        size[j] = 0;
    }

    let midx = AtomicU32::new(0);
    let midy = AtomicU32::new(0);
    let mut strategies: Vec<Strategy> = Vec::new();

    let initial_x: Vec<u32> = initial_x
        .iter()
        .map(|v| v.load(Ordering::Relaxed))
        .collect();
    let initial_y: Vec<u32> = initial_y
        .iter()
        .map(|v| v.load(Ordering::Relaxed))
        .collect();

    let shared_nodes_slice: &[Node] = match &shared_nodes_map {
        Some(m) => {
            // SAFETY: the map is page-aligned, Node is a repr(C) u64, and the
            // file holds nodepos bytes of nodes that do not change while mapped.
            unsafe {
                std::slice::from_raw_parts(m.as_ptr() as *const Node, nodepos as usize / NODE_SIZE)
            }
        }
        None => &[],
    };

    let unidecode_data = UNIDECODE_DATA.read().unwrap().clone();
    let drop_by_attribute_as_needed_attribute = DROP_BY_ATTRIBUTE_AS_NEEDED_ATTRIBUTE
        .read()
        .unwrap()
        .clone();

    let written = traverse_zooms(
        &mut fd,
        &mut size,
        &stringpool,
        &midx,
        &midy,
        &mut maxzoom,
        minzoom,
        outdb.as_ref(),
        outdir,
        buffer,
        fname,
        tmpdir,
        gamma,
        full_detail,
        low_detail,
        min_detail,
        &pool_off,
        &initial_x,
        &initial_y,
        SIMPLIFICATION.load(),
        MAXZOOM_SIMPLIFICATION.load(),
        &mut layermaps,
        prefilter,
        postfilter,
        attribute_accum,
        filter.map(|f| f.root()),
        &mut strategies,
        iz as i32,
        shared_nodes_slice,
        nodepos as usize,
        &shared_nodes_bloom,
        basezoom,
        droprate,
        &unidecode_data,
        Some(&drop_by_attribute_as_needed_attribute),
        DROP_BY_ATTRIBUTE_DESCENDING.load(Ordering::Relaxed),
    );

    if maxzoom != written {
        if written > minzoom {
            eprintln!(
                "\n\n\n*** NOTE TILES ONLY COMPLETE THROUGH ZOOM {} ***",
                written
            );
            maxzoom = written;
            ret = EXIT_INCOMPLETE;
        } else {
            eprintln!("{}: No zoom levels were successfully written", av());
            std::process::exit(EXIT_NODATA);
        }
    }

    drop(stringpool);
    if close_fd(poolfd) < 0 {
        perror("close pool");
    }

    drop(shared_nodes_map);
    drop(shared_nodes);

    // mbtiles-style bounding box and center
    let midx = midx.load(Ordering::Relaxed);
    let midy = midy.load(Ordering::Relaxed);

    let (mut minlon, mut maxlat) = tile2lonlat(midx as i64, midy as i64, maxzoom);
    let (mut maxlon, mut minlat) = tile2lonlat(
        midx.wrapping_add(1) as i64,
        midy.wrapping_add(1) as i64,
        maxzoom,
    );

    let mut midlat = (maxlat + minlat) / 2.0;
    let mut midlon = (maxlon + minlon) / 2.0;

    (minlon, maxlat) = tile2lonlat(file_bbox[0], file_bbox[1], 32);
    (maxlon, minlat) = tile2lonlat(file_bbox[2], file_bbox[3], 32);

    if midlat < minlat {
        midlat = minlat;
    }
    if midlat > maxlat {
        midlat = maxlat;
    }
    if midlon < minlon {
        midlon = minlon;
    }
    if midlon > maxlon {
        midlon = maxlon;
    }

    // antimeridian-aware bounding box
    let minlat2: f64;
    let minlon2: f64;
    let maxlat2: f64;
    let maxlon2: f64;
    // choose whichever of the two calculated bboxes is narrower
    if file_bbox2[2] - file_bbox2[0] < file_bbox1[2] - file_bbox1[0] {
        (minlon2, maxlat2) = tile2lonlat(file_bbox2[0], file_bbox2[1], 32);
        (maxlon2, minlat2) = tile2lonlat(file_bbox2[2], file_bbox2[3], 32);
    } else {
        (minlon2, maxlat2) = tile2lonlat(file_bbox1[0], file_bbox1[1], 32);
        (maxlon2, minlat2) = tile2lonlat(file_bbox1[2], file_bbox1[3], 32);
    }

    let mut merged_lm = merge_layermaps(&layermaps);

    for v in merged_lm.values_mut() {
        v.minzoom = minzoom;
        v.maxzoom = maxzoom;
    }

    let m = make_metadata(
        fname,
        minzoom,
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
        attribution,
        &merged_lm,
        true,
        description,
        prevent(P_TILE_STATS) == 0,
        attribute_descriptions,
        "tippecanoe",
        commandline,
        &strategies,
        basezoom,
        droprate,
        get(&RETAIN_POINTS_MULTIPLIER),
    );
    match outdb.as_ref() {
        Some(db) => mbtiles_write_metadata(&db.lock().unwrap(), &m, forcetable != 0),
        None => dir_write_metadata(outdir.unwrap_or(""), &m),
    }

    (ret, m)
}

fn has_name(long_options: &[LongOption], pl: &'static AtomicI32) -> bool {
    for lo in long_options {
        if let Some(f) = lo.flag {
            if std::ptr::eq(f, pl) {
                return true;
            }
        }
    }

    false
}

pub fn set_attribute_type(attribute_types: &mut HashMap<String, i32>, arg: &str) {
    let arg = cstr(arg);
    let s = match arg.find(':') {
        Some(s) => s,
        None => {
            eprintln!("-T{} option must be in the form -Tname:type", arg);
            std::process::exit(EXIT_ARGS);
        }
    };

    let name = arg[..s].to_string();
    let type_ = &arg[s + 1..];
    let t: i32;

    if type_ == "int" {
        t = MVT_INT;
    } else if type_ == "float" {
        t = MVT_FLOAT;
    } else if type_ == "string" {
        t = MVT_STRING;
    } else if type_ == "bool" {
        t = MVT_BOOL;
    } else {
        eprintln!(
            "Attribute type ({}) must be int, float, string, or bool",
            type_
        );
        std::process::exit(EXIT_ARGS);
    }

    attribute_types.entry(name).or_insert(t);
}

pub fn set_attribute_value(arg: &str) {
    let arg = cstr(arg);
    if arg.starts_with('{') {
        let mut jp = JsonPull::begin_string(arg.as_bytes());
        let o = match jp.read_tree() {
            Some(o) => o,
            None => {
                eprintln!(
                    "{}: --set-attribute {}: {}",
                    av(),
                    arg,
                    jp.error.unwrap_or("(null)")
                );
                std::process::exit(EXIT_JSON);
            }
        };

        let root = o.root();
        if root.type_() != JsonType::Hash {
            eprintln!("{}: --set-attribute {}: not a JSON object", av(), arg);
            std::process::exit(EXIT_JSON);
        }

        let mut set_attributes = SET_ATTRIBUTES.write().unwrap();
        for (i, (key, value)) in root.entries().into_iter().enumerate() {
            if key.type_() != JsonType::String {
                eprintln!("{}: --set-attribute {}: key {} not a string", av(), arg, i);
                std::process::exit(EXIT_JSON);
            }

            let val = stringify_value(Some(value), "json", 1, root);
            set_attributes
                .entry(key.string().to_string())
                .or_insert(val);
        }

        return;
    }

    let s = match arg.find(':') {
        Some(s) => s,
        None => {
            eprintln!(
                "--set-attribute {} option must be in the form --set-attribute name:value",
                arg
            );
            std::process::exit(EXIT_ARGS);
        }
    };

    let name = arg[..s].to_string();
    let value = arg[s + 1..].to_string();

    let mut val = SerialVal::default();
    let first = value.bytes().next().unwrap_or(0);
    if first.is_ascii_digit() || first == b'-' {
        val.type_ = MVT_DOUBLE;
    } else {
        val.type_ = MVT_STRING;
    }

    val.s = value;
    SET_ATTRIBUTES.write().unwrap().entry(name).or_insert(val);
}

fn parse_json_source(arg: &str, src: &mut Source) {
    let arg = cstr(arg);
    let mut jp = JsonPull::begin_string(arg.as_bytes());
    let o = match jp.read_tree() {
        Some(o) => o,
        None => {
            eprintln!("{}: -L{}: {}", av(), arg, jp.error.unwrap_or("(null)"));
            std::process::exit(EXIT_JSON);
        }
    };

    let root = o.root();
    if root.type_() != JsonType::Hash {
        eprintln!("{}: -L{}: not a JSON object", av(), arg);
        std::process::exit(EXIT_JSON);
    }

    match root.hash_get("file") {
        Some(fname) if fname.type_() == JsonType::String => {
            src.file = fname.string().to_string();
        }
        _ => {
            eprintln!("{}: -L{}: requires \"file\": filename", av(), arg);
            std::process::exit(EXIT_JSON);
        }
    }

    if let Some(layer) = root.hash_get("layer") {
        if layer.type_() == JsonType::String {
            src.layer = layer.string().to_string();
        }
    }

    if let Some(description) = root.hash_get("description") {
        if description.type_() == JsonType::String {
            src.description = description.string().to_string();
        }
    }

    if let Some(format) = root.hash_get("format") {
        if format.type_() == JsonType::String {
            src.format = format.string().to_string();
        }
    }
}

const fn heading(name: &'static str) -> LongOption {
    LongOption {
        name,
        has_arg: 0,
        flag: None,
        val: 0,
    }
}

const fn opt(name: &'static str, has_arg: i32, val: u8) -> LongOption {
    LongOption {
        name,
        has_arg,
        flag: None,
        val: val as i32,
    }
}

const fn flag(name: &'static str, f: &'static AtomicI32) -> LongOption {
    LongOption {
        name,
        has_arg: NO_ARGUMENT,
        flag: Some(f),
        val: 1,
    }
}

const REQ: i32 = REQUIRED_ARGUMENT;
const NO: i32 = NO_ARGUMENT;

fn long_options_orig() -> Vec<LongOption> {
    vec![
        heading("Output tileset"),
        opt("output", REQ, b'o'),
        opt("output-to-directory", REQ, b'e'),
        opt("force", NO, b'f'),
        opt("allow-existing", NO, b'F'),
        heading("Tileset description and attribution"),
        opt("name", REQ, b'n'),
        opt("attribution", REQ, b'A'),
        opt("description", REQ, b'N'),
        heading("Input files and layer names"),
        opt("layer", REQ, b'l'),
        opt("named-layer", REQ, b'L'),
        heading("Parallel processing of input"),
        opt("read-parallel", NO, b'P'),
        heading("Projection of input"),
        opt("projection", REQ, b's'),
        heading("Zoom levels"),
        opt("maximum-zoom", REQ, b'z'),
        opt("minimum-zoom", REQ, b'Z'),
        opt("smallest-maximum-zoom-guess", REQ, b'~'),
        flag(
            "extend-zooms-if-still-dropping",
            &ADDITIONAL[A_EXTEND_ZOOMS],
        ),
        opt("extend-zooms-if-still-dropping-maximum", REQ, b'~'),
        flag(
            "generate-variable-depth-tile-pyramid",
            &ADDITIONAL[A_VARIABLE_DEPTH_PYRAMID],
        ),
        opt("one-tile", REQ, b'R'),
        heading("Tile resolution"),
        opt("full-detail", REQ, b'd'),
        opt("low-detail", REQ, b'D'),
        opt("minimum-detail", REQ, b'm'),
        opt("extra-detail", REQ, b'~'),
        heading("Filtering feature attributes"),
        opt("exclude", REQ, b'x'),
        opt("include", REQ, b'y'),
        opt("exclude-all", NO, b'X'),
        heading("Modifying feature attributes"),
        opt("attribute-type", REQ, b'T'),
        opt("attribute-description", REQ, b'Y'),
        opt("accumulate-attribute", REQ, b'E'),
        flag("empty-csv-columns-are-null", &PREVENT[P_EMPTY_CSV_COLUMNS]),
        flag(
            "convert-stringified-ids-to-numbers",
            &ADDITIONAL[A_CONVERT_NUMERIC_IDS],
        ),
        opt("use-attribute-for-id", REQ, b'~'),
        flag("single-precision", &PREVENT[P_SINGLE_PRECISION]),
        opt("set-attribute", REQ, b'~'),
        opt("maximum-string-attribute-length", REQ, b'~'),
        heading("Filtering features by attributes"),
        opt("feature-filter-file", REQ, b'J'),
        opt("feature-filter", REQ, b'j'),
        heading("Dropping a fixed fraction of features by zoom level"),
        opt("drop-rate", REQ, b'r'),
        opt("retain-points-multiplier", REQ, b'~'),
        opt("base-zoom", REQ, b'B'),
        opt("drop-denser", REQ, b'~'),
        flag(
            "limit-base-zoom-to-maximum-zoom",
            &PREVENT[P_BASEZOOM_ABOVE_MAXZOOM],
        ),
        flag("drop-lines", &ADDITIONAL[A_LINE_DROP]),
        flag("drop-polygons", &ADDITIONAL[A_POLYGON_DROP]),
        opt("cluster-distance", REQ, b'K'),
        opt("cluster-maxzoom", REQ, b'k'),
        opt("preserve-point-density-threshold", REQ, b'~'),
        opt("preserve-multiplier-density-threshold", REQ, b'~'),
        heading("Dropping or merging a fraction of features to keep under tile size limits"),
        flag(
            "drop-densest-as-needed",
            &ADDITIONAL[A_DROP_DENSEST_AS_NEEDED],
        ),
        flag(
            "drop-fraction-as-needed",
            &ADDITIONAL[A_DROP_FRACTION_AS_NEEDED],
        ),
        flag(
            "drop-smallest-as-needed",
            &ADDITIONAL[A_DROP_SMALLEST_AS_NEEDED],
        ),
        opt("drop-by-attribute-as-needed", REQ, b'~'),
        opt("drop-by-attribute-order", REQ, b'~'),
        flag(
            "coalesce-densest-as-needed",
            &ADDITIONAL[A_COALESCE_DENSEST_AS_NEEDED],
        ),
        flag(
            "coalesce-fraction-as-needed",
            &ADDITIONAL[A_COALESCE_FRACTION_AS_NEEDED],
        ),
        flag(
            "coalesce-smallest-as-needed",
            &ADDITIONAL[A_COALESCE_SMALLEST_AS_NEEDED],
        ),
        flag("force-feature-limit", &PREVENT[P_DYNAMIC_DROP]),
        flag(
            "cluster-densest-as-needed",
            &ADDITIONAL[A_CLUSTER_DENSEST_AS_NEEDED],
        ),
        flag(
            "keep-point-cluster-position",
            &ADDITIONAL[A_KEEP_POINT_CLUSTER_POSITION],
        ),
        heading("Dropping tightly overlapping features"),
        opt("gamma", REQ, b'g'),
        flag(
            "increase-gamma-as-needed",
            &ADDITIONAL[A_INCREASE_GAMMA_AS_NEEDED],
        ),
        heading("Line and polygon simplification"),
        opt("simplification", REQ, b'S'),
        flag("no-line-simplification", &PREVENT[P_SIMPLIFY]),
        flag("simplify-only-low-zooms", &PREVENT[P_SIMPLIFY_LOW]),
        opt("simplification-at-maximum-zoom", REQ, b'~'),
        flag(
            "no-tiny-polygon-reduction",
            &PREVENT[P_TINY_POLYGON_REDUCTION],
        ),
        flag(
            "no-tiny-polygon-reduction-at-maximum-zoom",
            &PREVENT[P_TINY_POLYGON_REDUCTION_AT_MAXZOOM],
        ),
        opt("tiny-polygon-size", REQ, b'~'),
        flag(
            "no-simplification-of-shared-nodes",
            &PREVENT[P_SIMPLIFY_SHARED_NODES],
        ),
        flag("visvalingam", &ADDITIONAL[A_VISVALINGAM]),
        heading("Attempts to improve shared polygon boundaries"),
        flag(
            "detect-shared-borders",
            &ADDITIONAL[A_DETECT_SHARED_BORDERS],
        ),
        flag("grid-low-zooms", &ADDITIONAL[A_GRID_LOW_ZOOMS]),
        heading("Controlling clipping to tile boundaries"),
        opt("buffer", REQ, b'b'),
        flag("no-clipping", &PREVENT[P_CLIPPING]),
        flag("no-duplication", &PREVENT[P_DUPLICATION]),
        heading("Reordering features within each tile"),
        flag("preserve-input-order", &PREVENT[P_INPUT_ORDER]),
        flag("reorder", &ADDITIONAL[A_REORDER]),
        flag("coalesce", &ADDITIONAL[A_COALESCE]),
        flag("reverse", &ADDITIONAL[A_REVERSE]),
        flag("hilbert", &ADDITIONAL[A_HILBERT]),
        opt("order-by", REQ, b'~'),
        opt("order-descending-by", REQ, b'~'),
        opt("order-smallest-first", NO, b'~'),
        opt("order-largest-first", NO, b'~'),
        heading("Adding calculated attributes"),
        flag(
            "calculate-feature-density",
            &ADDITIONAL[A_CALCULATE_FEATURE_DENSITY],
        ),
        flag("generate-ids", &ADDITIONAL[A_GENERATE_IDS]),
        flag("calculate-feature-index", &ADDITIONAL[A_CALCULATE_INDEX]),
        heading("Trying to correct bad source geometry"),
        flag(
            "detect-longitude-wraparound",
            &ADDITIONAL[A_DETECT_WRAPAROUND],
        ),
        flag(
            "use-source-polygon-winding",
            &PREVENT[P_USE_SOURCE_POLYGON_WINDING],
        ),
        flag(
            "reverse-source-polygon-winding",
            &PREVENT[P_REVERSE_SOURCE_POLYGON_WINDING],
        ),
        opt("clip-bounding-box", REQ, b'~'),
        flag(
            "convert-polygons-to-label-points",
            &ADDITIONAL[A_GENERATE_POLYGON_LABEL_POINTS],
        ),
        heading("Filtering tile contents"),
        opt("prefilter", REQ, b'C'),
        opt("postfilter", REQ, b'c'),
        heading("Setting or disabling tile size limits"),
        opt("maximum-tile-bytes", REQ, b'M'),
        opt("maximum-tile-features", REQ, b'O'),
        opt("limit-tile-feature-count", REQ, b'~'),
        opt("limit-tile-feature-count-at-maximum-zoom", REQ, b'~'),
        flag("no-feature-limit", &PREVENT[P_FEATURE_LIMIT]),
        flag("no-tile-size-limit", &PREVENT[P_KILOBYTE_LIMIT]),
        flag("no-tile-compression", &PREVENT[P_TILE_COMPRESSION]),
        flag("no-tile-stats", &PREVENT[P_TILE_STATS]),
        opt("tile-stats-attributes-limit", REQ, b'~'),
        opt("tile-stats-sample-values-limit", REQ, b'~'),
        opt("tile-stats-values-limit", REQ, b'~'),
        heading("Temporary storage"),
        opt("temporary-directory", REQ, b't'),
        heading("Progress indicator"),
        opt("quiet", NO, b'q'),
        opt("no-progress-indicator", NO, b'Q'),
        opt("progress-interval", REQ, b'U'),
        opt("json-progress", NO, b'u'),
        heading("Version"),
        opt("version", NO, b'v'),
        heading(""),
        opt("prevent", REQ, b'p'),
        opt("additional", REQ, b'a'),
        flag("check-polygons", &ADDITIONAL[A_DEBUG_POLYGON]),
        flag("no-polygon-splitting", &PREVENT[P_POLYGON_SPLIT]),
        flag("prefer-radix-sort", &ADDITIONAL[A_PREFER_RADIX_SORT]),
        opt("unidecode-data", REQ, b'~'),
        opt("help", NO, b'H'),
    ]
}

fn usage(argv: &[String], status: i32) -> ! {
    let forms = ["[options] [file.json ...]"];
    let required = [
        UsageRequiredOption {
            name: "output",
            placeholder: "output.mbtiles",
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
        &long_options_orig(),
        &required,
    );
    std::process::exit(status);
}

/// libc `sscanf` with a format of `%lf` conversions. Returns the number of
/// values converted.
fn sscanf_doubles(s: &str, format: &str, out: &mut [f64; 4]) -> i32 {
    let (Ok(cs), Ok(cf)) = (
        std::ffi::CString::new(cstr(s)),
        std::ffi::CString::new(format),
    ) else {
        return 0;
    };
    let [a, b, c, d] = out;
    // SAFETY: the format has four %lf conversions, each given a pointer to an f64.
    unsafe {
        libc::sscanf(
            cs.as_ptr(),
            cf.as_ptr(),
            a as *mut f64,
            b as *mut f64,
            c as *mut f64,
            d as *mut f64,
        )
    }
}

/// libc `sscanf` with a format of three `%u` conversions.
fn sscanf_unsigneds(s: &str, format: &str, out: &mut [u32; 3]) -> i32 {
    let (Ok(cs), Ok(cf)) = (
        std::ffi::CString::new(cstr(s)),
        std::ffi::CString::new(format),
    ) else {
        return 0;
    };
    let [a, b, c] = out;
    // SAFETY: the format has three %u conversions, each given a pointer to a u32.
    unsafe {
        libc::sscanf(
            cs.as_ptr(),
            cf.as_ptr(),
            a as *mut u32,
            b as *mut u32,
            c as *mut u32,
        )
    }
}

/// `fprintf(stderr, "...%c\n", c)` for a byte that may not be ASCII.
fn eprint_bytes(parts: &[&[u8]]) {
    let mut e = std::io::stderr().lock();
    for p in parts {
        let _ = e.write_all(p);
    }
}

/// The `tippecanoe` program.
pub fn main() {
    let mut argv: Vec<String> = std::env::args().collect();
    let argc = argv.len();

    if argc == 1 {
        // with no arguments at all, there is nothing to complain about
        // specifically, so say in general what the arguments should be
        usage(&argv, EXIT_ARGS);
    }

    let _ = AV.set(argv[0].clone());
    init_cpus();

    let mut i: i32;

    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut layername: Option<String> = None;
    let mut out_mbtiles: Option<String> = None;
    let mut out_dir: Option<String> = None;
    let mut outdb: Option<Mutex<Connection>> = None;
    let mut maxzoom: i32 = 14;
    let mut minzoom: i32 = 0;
    let mut basezoom: i32 = -1;
    let mut basezoom_marker_width: f64 = 1.0;
    let mut force = 0;
    let mut forcetable = 0;
    let mut droprate: f64 = 2.5;
    let mut gamma: f64 = 0.0;
    let mut buffer: i32 = 5;
    let mut tmpdir: String = crate::platform::default_tmpdir();
    let mut attribution: Option<String> = None;
    let mut sources: Vec<Source> = Vec::new();
    let mut prefilter: Option<String> = None;
    let mut postfilter: Option<String> = None;
    let mut guess_maxzoom = false;
    let mut minimum_maxzoom: i32 = 0;
    let mut guess_cluster_maxzoom = false;

    let mut exclude: BTreeSet<String> = BTreeSet::new();
    let mut include: BTreeSet<String> = BTreeSet::new();
    let mut attribute_types: HashMap<String, i32> = HashMap::new();
    let mut attribute_accum: HashMap<String, AttributeOp> = HashMap::new();
    let mut attribute_descriptions: BTreeMap<String, String> = BTreeMap::new();
    let mut exclude_all = 0;
    let mut read_parallel = 0;
    let files_open_at_start: i32;
    let mut filter: Option<JsonTree> = None;

    MEMSIZE.store(calc_memsize(), Ordering::Relaxed);

    for i in 0..256 {
        set_prevent(i, 0);
        set_additional(i, 0);
    }

    let long_options_orig = long_options_orig();
    let long_options = strip_usage_headings(&long_options_orig);

    {
        for lo in &long_options {
            if let Some(f) = lo.flag {
                if f.load(Ordering::Relaxed) != 0 {
                    eprintln!("Internal error: reused {}", lo.name);
                    std::process::exit(EXIT_IMPOSSIBLE);
                }
                f.store(1, Ordering::Relaxed);
            }
        }

        for lo in &long_options {
            if let Some(f) = lo.flag {
                f.store(0, Ordering::Relaxed);
            }
        }
    }

    let commandline = format_commandline(&argv);
    let getopt_str = getopt_string(&long_options);

    let mut g = Getopt::new();
    let mut option_index: usize = 0;
    loop {
        i = g.getopt_long(
            &mut argv,
            &getopt_str,
            &long_options,
            Some(&mut option_index),
        );
        if i == -1 {
            break;
        }
        let optarg: String = g.optarg.clone().unwrap_or_default();

        match i {
            0 => {}

            0x7E /* '~' */ => {
                let opt = long_options[option_index].name;
                if opt == "tile-stats-attributes-limit" {
                    MAX_TILESTATS_ATTRIBUTES.store(atoi(&optarg) as usize, Ordering::Relaxed);
                } else if opt == "tile-stats-sample-values-limit" {
                    MAX_TILESTATS_SAMPLE_VALUES.store(atoi(&optarg) as usize, Ordering::Relaxed);
                } else if opt == "tile-stats-values-limit" {
                    MAX_TILESTATS_VALUES.store(atoi(&optarg) as usize, Ordering::Relaxed);
                } else if opt == "clip-bounding-box" {
                    let mut v = [0.0f64; 4];
                    if sscanf_doubles(&optarg, "%lf,%lf,%lf,%lf", &mut v) == 4 {
                        let clip = ClipBbox {
                            lon1: v[0],
                            lat1: v[1],
                            lon2: v[2],
                            lat2: v[3],
                            ..Default::default()
                        };
                        CLIPBBOXES.write().unwrap().push(clip);
                    } else {
                        eprintln!(
                            "{}: Can't parse bounding box --{}={}",
                            argv[0],
                            opt,
                            cstr(&optarg)
                        );
                        std::process::exit(EXIT_ARGS);
                    }
                } else if opt == "use-attribute-for-id" {
                    *ATTRIBUTE_FOR_ID.write().unwrap() = cstr(&optarg).to_string();
                } else if opt == "set-attribute" {
                    set_attribute_value(&optarg);
                } else if opt == "smallest-maximum-zoom-guess" {
                    maxzoom = MAX_ZOOM;
                    guess_maxzoom = true;
                    minimum_maxzoom = atoi_require(&optarg, "Minimum maxzoom");
                    if minimum_maxzoom > MAX_ZOOM {
                        eprintln!(
                            "{}: {}: minimum maxzoom can be at most {}",
                            argv[0],
                            cstr(&optarg),
                            MAX_ZOOM
                        );
                        std::process::exit(EXIT_ARGS);
                    }
                } else if opt == "tiny-polygon-size" {
                    TINY_POLYGON_SIZE.store(atoi(&optarg), Ordering::Relaxed);
                } else if opt == "extra-detail" {
                    EXTRA_DETAIL.store(atoi_require(&optarg, "Extra detail"), Ordering::Relaxed);
                    if get(&EXTRA_DETAIL) > 30 {
                        // So the maximum geometry delta of just under 2 tile extents
                        // is less than 2^31

                        eprintln!("{}: --extra-detail can be at most 30", argv[0]);
                        std::process::exit(EXIT_ARGS);
                    }
                } else if opt == "order-by" {
                    ORDER_BY
                        .write()
                        .unwrap()
                        .push(OrderField::new(cstr(&optarg).to_string(), false));
                } else if opt == "order-descending-by" {
                    ORDER_BY
                        .write()
                        .unwrap()
                        .push(OrderField::new(cstr(&optarg).to_string(), true));
                } else if opt == "order-smallest-first" {
                    ORDER_BY
                        .write()
                        .unwrap()
                        .push(OrderField::new(ORDER_BY_SIZE.to_string(), false));
                    ORDER_BY_SIZE_ENABLED.store(true, Ordering::Relaxed);
                } else if opt == "order-largest-first" {
                    ORDER_BY
                        .write()
                        .unwrap()
                        .push(OrderField::new(ORDER_BY_SIZE.to_string(), true));
                    ORDER_BY_SIZE_ENABLED.store(true, Ordering::Relaxed);
                } else if opt == "drop-by-attribute-as-needed" {
                    if cstr_len(&optarg) == 0 {
                        eprintln!(
                            "{}: --drop-by-attribute-as-needed requires a non-empty attribute name",
                            argv[0]
                        );
                        std::process::exit(EXIT_ARGS);
                    }
                    *DROP_BY_ATTRIBUTE_AS_NEEDED_ATTRIBUTE.write().unwrap() =
                        cstr(&optarg).to_string();
                    set_additional(A_DROP_BY_ATTRIBUTE_AS_NEEDED, 1);
                } else if opt == "drop-by-attribute-order" {
                    if cstr(&optarg) == "asc" {
                        DROP_BY_ATTRIBUTE_DESCENDING.store(false, Ordering::Relaxed);
                    } else if cstr(&optarg) == "desc" {
                        DROP_BY_ATTRIBUTE_DESCENDING.store(true, Ordering::Relaxed);
                    } else {
                        eprintln!(
                            "{}: {}: order must be one of: asc, desc",
                            argv[0],
                            cstr(&optarg)
                        );
                        std::process::exit(EXIT_ARGS);
                    }
                } else if opt == "simplification-at-maximum-zoom" {
                    MAXZOOM_SIMPLIFICATION
                        .store(atof_require(&optarg, "Mazoom simplification"));
                    if MAXZOOM_SIMPLIFICATION.load() <= 0.0 {
                        eprintln!(
                            "{}: --simplification-at-maximum-zoom must be > 0",
                            argv[0]
                        );
                        std::process::exit(EXIT_ARGS);
                    }
                } else if opt == "limit-tile-feature-count" {
                    LIMIT_TILE_FEATURE_COUNT.store(
                        atoll_require(&optarg, "Limit tile feature count") as usize,
                        Ordering::Relaxed,
                    );
                } else if opt == "limit-tile-feature-count-at-maximum-zoom" {
                    LIMIT_TILE_FEATURE_COUNT_AT_MAXZOOM.store(
                        atoll_require(&optarg, "Limit tile feature count at maxzoom") as usize,
                        Ordering::Relaxed,
                    );
                } else if opt == "drop-denser" {
                    DROP_DENSER.store(
                        atoi_require(&optarg, "Drop denser rate") as u32,
                        Ordering::Relaxed,
                    );
                    if DROP_DENSER.load(Ordering::Relaxed) > 100 {
                        eprintln!("{}: --drop-denser can be at most 100", argv[0]);
                        std::process::exit(EXIT_ARGS);
                    }
                } else if opt == "preserve-point-density-threshold" {
                    PRESERVE_POINT_DENSITY_THRESHOLD.store(
                        atoll_require(&optarg, "Preserve point density threshold") as u64,
                        Ordering::Relaxed,
                    );
                } else if opt == "preserve-multiplier-density-threshold" {
                    PRESERVE_MULTIPLIER_DENSITY_THRESHOLD.store(
                        atoll_require(&optarg, "Preserve multiplier density threshold") as u64,
                        Ordering::Relaxed,
                    );
                } else if opt == "extend-zooms-if-still-dropping-maximum" {
                    EXTEND_ZOOMS_MAX.store(
                        atoll_require(&optarg, "Maximum number by which to extend zooms"),
                        Ordering::Relaxed,
                    );
                } else if opt == "retain-points-multiplier" {
                    RETAIN_POINTS_MULTIPLIER.store(
                        atoll_require(
                            &optarg,
                            "Multiply the fraction of points retained by zoom level",
                        ) as i32,
                        Ordering::Relaxed,
                    );
                } else if opt == "unidecode-data" {
                    *UNIDECODE_DATA.write().unwrap() = read_unidecode(cstr(&optarg));
                } else if opt == "maximum-string-attribute-length" {
                    MAXIMUM_STRING_ATTRIBUTE_LENGTH.store(
                        atoll_require(&optarg, "Maximum string attribute length") as usize,
                        Ordering::Relaxed,
                    );
                } else {
                    eprintln!("{}: Unrecognized option --{}", argv[0], opt);
                    std::process::exit(EXIT_ARGS);
                }
            }

            0x6E /* 'n' */ => {
                name = Some(cstr(&optarg).to_string());
            }

            0x4E /* 'N' */ => {
                description = Some(cstr(&optarg).to_string());
            }

            0x6C /* 'l' */ => {
                layername = Some(cstr(&optarg).to_string());
            }

            0x41 /* 'A' */ => {
                attribution = Some(cstr(&optarg).to_string());
            }

            0x4C /* 'L' */ => {
                let mut src = Source::default();
                if optarg.starts_with('{') {
                    parse_json_source(&optarg, &mut src);
                } else {
                    let arg = cstr(&optarg);
                    match arg.find(':') {
                        Some(cp) if cp != 0 => {
                            src.layer = arg[..cp].to_string();
                            src.file = arg[cp + 1..].to_string();
                        }
                        _ => {
                            eprintln!("{}: -L requires layername:file", argv[0]);
                            std::process::exit(EXIT_ARGS);
                        }
                    }
                }
                sources.push(src);
            }

            0x7A /* 'z' */ => {
                if cstr(&optarg) == "g" {
                    maxzoom = MAX_ZOOM;
                    guess_maxzoom = true;
                } else {
                    maxzoom = atoi_require(&optarg, "Maxzoom");
                }
            }

            0x5A /* 'Z' */ => {
                minzoom = atoi_require(&optarg, "Minzoom");
            }

            0x52 /* 'R' */ => {
                let mut v = [0u32; 3];
                if sscanf_unsigneds(&optarg, "%u/%u/%u", &mut v) == 3 {
                    minzoom = v[0] as i32;
                    maxzoom = v[0] as i32;
                    JUSTX.store(v[1] as i64, Ordering::Relaxed);
                    JUSTY.store(v[2] as i64, Ordering::Relaxed);
                } else {
                    eprintln!("--one-tile argument must be z/x/y");
                    std::process::exit(EXIT_ARGS);
                }
            }

            0x42 /* 'B' */ => {
                let first = optarg.bytes().next().unwrap_or(0);
                if cstr(&optarg) == "g" {
                    basezoom = -2;
                } else if first == b'g' || first == b'f' {
                    basezoom = -2;
                    if first == b'g' {
                        basezoom_marker_width = atof_require(&optarg[1..], "Marker width");
                    } else {
                        basezoom_marker_width =
                            (50000.0 / atof_require(&optarg[1..], "Marker width")).sqrt();
                    }
                    if basezoom_marker_width == 0.0
                        || atof_require(&optarg[1..], "Marker width") == 0.0
                    {
                        eprint_bytes(&[
                            argv[0].as_bytes(),
                            b": Must specify value >0 with -B",
                            &[first],
                            b"\n",
                        ]);
                        std::process::exit(EXIT_ARGS);
                    }
                } else {
                    basezoom = atoi_require(&optarg, "Basezoom");
                    if basezoom == 0 && cstr(&optarg) != "0" {
                        eprintln!("{}: Couldn't understand -B{}", argv[0], cstr(&optarg));
                        std::process::exit(EXIT_ARGS);
                    }
                }
            }

            0x4B /* 'K' */ => {
                CLUSTER_DISTANCE.store(
                    atoi_require(&optarg, "Cluster distance"),
                    Ordering::Relaxed,
                );
                if get(&CLUSTER_DISTANCE) > 255 {
                    eprintln!(
                        "{}: --cluster-distance {} is too big; limit is 255",
                        argv[0],
                        get(&CLUSTER_DISTANCE)
                    );
                    std::process::exit(EXIT_ARGS);
                }
            }

            0x6B /* 'k' */ => {
                if cstr(&optarg) == "g" {
                    CLUSTER_MAXZOOM.store(MAX_ZOOM - 1, Ordering::Relaxed);
                    guess_cluster_maxzoom = true;
                } else {
                    CLUSTER_MAXZOOM.store(
                        atoi_require(&optarg, "Cluster maxzoom"),
                        Ordering::Relaxed,
                    );
                }
            }

            0x64 /* 'd' */ => {
                FULL_DETAIL.store(atoi_require(&optarg, "Full detail"), Ordering::Relaxed);
                if FULL_DETAIL.load(Ordering::Relaxed) > 30 {
                    // So the maximum geometry delta of just under 2 tile extents
                    // is less than 2^31

                    eprintln!("{}: --full-detail can be at most 30", argv[0]);
                    std::process::exit(EXIT_ARGS);
                }
            }

            0x44 /* 'D' */ => {
                LOW_DETAIL.store(atoi_require(&optarg, "Low detail"), Ordering::Relaxed);
                if LOW_DETAIL.load(Ordering::Relaxed) > 30 {
                    eprintln!("{}: --low-detail can be at most 30", argv[0]);
                    std::process::exit(EXIT_ARGS);
                }
            }

            0x6D /* 'm' */ => {
                MIN_DETAIL.store(atoi_require(&optarg, "Min detail"), Ordering::Relaxed);
            }

            0x6F /* 'o' */ => {
                if let Some(out) = &out_mbtiles {
                    eprintln!(
                        "{}: Can't specify both {} and {} as output",
                        argv[0],
                        out,
                        cstr(&optarg)
                    );
                    std::process::exit(EXIT_ARGS);
                }
                if let Some(out) = &out_dir {
                    eprintln!(
                        "{}: Can't specify both {} and {} as output",
                        argv[0],
                        out,
                        cstr(&optarg)
                    );
                    std::process::exit(EXIT_ARGS);
                }
                out_mbtiles = Some(cstr(&optarg).to_string());
            }

            0x65 /* 'e' */ => {
                if let Some(out) = &out_mbtiles {
                    eprintln!(
                        "{}: Can't specify both {} and {} as output",
                        argv[0],
                        out,
                        cstr(&optarg)
                    );
                    std::process::exit(EXIT_ARGS);
                }
                if let Some(out) = &out_dir {
                    eprintln!(
                        "{}: Can't specify both {} and {} as output",
                        argv[0],
                        out,
                        cstr(&optarg)
                    );
                    std::process::exit(EXIT_ARGS);
                }
                out_dir = Some(cstr(&optarg).to_string());
            }

            0x78 /* 'x' */ => {
                exclude.insert(cstr(&optarg).to_string());
            }

            0x79 /* 'y' */ => {
                exclude_all = 1;
                include.insert(cstr(&optarg).to_string());
            }

            0x58 /* 'X' */ => {
                exclude_all = 1;
            }

            0x59 /* 'Y' */ => {
                let arg = cstr(&optarg);
                match arg.find(':') {
                    Some(cp) if cp != 0 => {
                        let attrib = arg[..cp].to_string();
                        let desc = arg[cp + 1..].to_string();
                        attribute_descriptions.entry(attrib).or_insert(desc);
                    }
                    _ => {
                        eprintln!("{}: -Y requires attribute:description", argv[0]);
                        std::process::exit(EXIT_ARGS);
                    }
                }
            }

            0x4A /* 'J' */ => {
                filter = Some(read_filter(cstr(&optarg)));
            }

            0x6A /* 'j' */ => {
                filter = Some(parse_filter(cstr(&optarg)));
            }

            0x72 /* 'r' */ => {
                let first = optarg.bytes().next().unwrap_or(0);
                if cstr(&optarg) == "g" {
                    droprate = -2.0;
                } else if cstr(&optarg) == "p" {
                    droprate = -3.0;
                } else if first == b'g' || first == b'f' {
                    droprate = -2.0;
                    if first == b'g' {
                        basezoom_marker_width = atof_require(&optarg[1..], "Marker width");
                    } else {
                        basezoom_marker_width =
                            (50000.0 / atof_require(&optarg[1..], "Marker width")).sqrt();
                    }
                    if basezoom_marker_width == 0.0
                        || atof_require(&optarg[1..], "Marker width") == 0.0
                    {
                        eprint_bytes(&[
                            argv[0].as_bytes(),
                            b": Must specify value >0 with -r",
                            &[first],
                            b"\n",
                        ]);
                        std::process::exit(EXIT_ARGS);
                    }
                } else {
                    droprate = atof_require(&optarg, "Drop rate");
                }
            }

            0x62 /* 'b' */ => {
                buffer = atoi_require(&optarg, "Buffer");
                if buffer > 127 {
                    // So the maximum geometry delta is under 2 tile extents,
                    // from less than half a tile beyond one side to less than
                    // half a tile beyond the other.

                    eprintln!("{}: --buffer can be at most 127", argv[0]);
                    std::process::exit(EXIT_ARGS);
                }
            }

            0x66 /* 'f' */ => {
                force = 1;
            }

            0x46 /* 'F' */ => {
                forcetable = 1;
            }

            0x74 /* 't' */ => {
                tmpdir = cstr(&optarg).to_string();
                if !tmpdir.starts_with('/') {
                    eprintln!("Warning: temp directory {} doesn't begin with /", tmpdir);
                }
            }

            0x67 /* 'g' */ => {
                gamma = atof_require(&optarg, "Gamma");
            }

            0x71 /* 'q' */ => {
                QUIET.store(1, Ordering::Relaxed);
            }

            0x51 /* 'Q' */ => {
                QUIET_PROGRESS.store(1, Ordering::Relaxed);
            }

            0x75 /* 'u' */ => {
                QUIET.store(1, Ordering::Relaxed);
                LOGGER.json_enabled.store(true, Ordering::Relaxed);
            }

            0x55 /* 'U' */ => {
                PROGRESS_INTERVAL.store(atof_require(&optarg, "Progress interval"));
            }

            0x70 /* 'p' */ => {
                for &cp in cstr(&optarg).as_bytes() {
                    let c = (cp & 0xFF) as usize;
                    if has_name(&long_options, &PREVENT[c]) {
                        set_prevent(c, 1);
                    } else {
                        eprint_bytes(&[
                            argv[0].as_bytes(),
                            b": Unknown option -p",
                            &[cp],
                            b"\n",
                        ]);
                        std::process::exit(EXIT_ARGS);
                    }
                }
            }

            0x61 /* 'a' */ => {
                for &cp in cstr(&optarg).as_bytes() {
                    let c = (cp & 0xFF) as usize;
                    if has_name(&long_options, &ADDITIONAL[c]) {
                        set_additional(c, 1);
                    } else {
                        eprint_bytes(&[
                            argv[0].as_bytes(),
                            b": Unknown option -a",
                            &[cp],
                            b"\n",
                        ]);
                        std::process::exit(EXIT_ARGS);
                    }
                }
            }

            0x76 /* 'v' */ => {
                eprintln!("tippecanoe {}", version_str());
                std::process::exit(EXIT_SUCCESS);
            }

            0x50 /* 'P' */ => {
                read_parallel = 1;
            }

            0x73 /* 's' */ => {
                crate::projection::set_projection_or_exit(cstr(&optarg));
            }

            0x53 /* 'S' */ => {
                SIMPLIFICATION.store(atof_require(&optarg, "Simplification"));
                if SIMPLIFICATION.load() <= 0.0 {
                    eprintln!("{}: --simplification must be > 0", argv[0]);
                    std::process::exit(EXIT_ARGS);
                }
            }

            0x4D /* 'M' */ => {
                MAX_TILE_SIZE.store(
                    atoll_require(&optarg, "Max tile size") as usize,
                    Ordering::Relaxed,
                );
            }

            0x4F /* 'O' */ => {
                MAX_TILE_FEATURES.store(
                    atoll_require(&optarg, "Max tile features") as usize,
                    Ordering::Relaxed,
                );
            }

            0x63 /* 'c' */ => {
                postfilter = Some(cstr(&optarg).to_string());
            }

            0x43 /* 'C' */ => {
                prefilter = Some(cstr(&optarg).to_string());
            }

            0x54 /* 'T' */ => {
                set_attribute_type(&mut attribute_types, &optarg);
            }

            0x45 /* 'E' */ => {
                set_attribute_accum_arg(&mut attribute_accum, cstr(&optarg), &argv);
            }

            _ => {
                if i != b'H' as i32 && i != b'?' as i32 {
                    eprint_bytes(&[b"Unknown option -", &[i as u8], b"\n"]);
                }
                usage(
                    &argv,
                    if i == b'H' as i32 {
                        EXIT_SUCCESS
                    } else {
                        EXIT_ARGS
                    },
                );
            }
        }
    }

    set_index_hilbert(additional(A_HILBERT) != 0);

    // Wait until here to project the bounding box, so that the behavior is
    // the same no matter what order the projection and bounding box are
    // specified in
    for c in CLIPBBOXES.write().unwrap().iter_mut() {
        (c.minx, c.maxy) = (projection().project)(c.lon1, c.lat1, 32);
        (c.maxx, c.miny) = (projection().project)(c.lon2, c.lat2, 32);
    }

    if get(&MAX_TILESTATS_SAMPLE_VALUES) < get(&MAX_TILESTATS_VALUES) {
        MAX_TILESTATS_SAMPLE_VALUES.store(get(&MAX_TILESTATS_VALUES), Ordering::Relaxed);
    }

    crate::platform::sigpipe_ignore();

    files_open_at_start = open_fd(get_null_device(), libc::O_RDONLY | O_CLOEXEC, 0);
    if files_open_at_start < 0 {
        perror("open /dev/null");
        std::process::exit(EXIT_OPEN);
    }
    if close_fd(files_open_at_start) != 0 {
        perror("close");
        std::process::exit(EXIT_CLOSE);
    }

    if FULL_DETAIL.load(Ordering::Relaxed) <= 0 {
        FULL_DETAIL.store(12, Ordering::Relaxed);
    }

    if droprate == -3.0 && !guess_maxzoom {
        eprintln!("Can't use -rp without either -zg or --smallest-maximum-zoom-guess");
        std::process::exit(EXIT_ARGS);
    }

    if maxzoom > MAX_ZOOM {
        maxzoom = MAX_ZOOM;
        eprintln!("Highest supported zoom is -z{}", maxzoom);
    }

    // Need two checks: one for geometry representation, the other for
    // index traversal when guessing base zoom and drop rate

    // This previously dropped the maxzoom rather than the detail when they were in conflict,
    // which proved to be annoying.
    if !guess_maxzoom {
        if maxzoom > 32 - FULL_DETAIL.load(Ordering::Relaxed) {
            FULL_DETAIL.store(32 - maxzoom, Ordering::Relaxed);
            eprintln!(
                "Highest supported detail with maxzoom {} is {}",
                maxzoom,
                FULL_DETAIL.load(Ordering::Relaxed)
            );
        }
        if maxzoom > 33 - LOW_DETAIL.load(Ordering::Relaxed) {
            // that is, maxzoom - 1 > 32 - low_detail
            LOW_DETAIL.store(33 - maxzoom, Ordering::Relaxed);
            eprintln!(
                "Highest supported low detail with maxzoom {} is {}",
                maxzoom,
                LOW_DETAIL.load(Ordering::Relaxed)
            );
        }
    }
    if minzoom > maxzoom {
        eprintln!(
            "{}: Minimum zoom -Z{} cannot be greater than maxzoom -z{}",
            argv[0], minzoom, maxzoom
        );
        std::process::exit(EXIT_ARGS);
    }

    if FULL_DETAIL.load(Ordering::Relaxed) < MIN_DETAIL.load(Ordering::Relaxed) {
        MIN_DETAIL.store(FULL_DETAIL.load(Ordering::Relaxed), Ordering::Relaxed);
        eprintln!(
            "{}: Reducing minimum detail to match full detail {}",
            argv[0],
            MIN_DETAIL.load(Ordering::Relaxed)
        );
    }

    if LOW_DETAIL.load(Ordering::Relaxed) < MIN_DETAIL.load(Ordering::Relaxed) {
        MIN_DETAIL.store(LOW_DETAIL.load(Ordering::Relaxed), Ordering::Relaxed);
        eprintln!(
            "{}: Reducing minimum detail to match low detail {}",
            argv[0],
            MIN_DETAIL.load(Ordering::Relaxed)
        );
    }

    if basezoom == -1 {
        // basezoom unspecified
        if !guess_maxzoom {
            basezoom = maxzoom;
        }
    }

    if get(&EXTRA_DETAIL) >= 0
        || prevent(P_SIMPLIFY_SHARED_NODES) != 0
        || additional(A_EXTEND_ZOOMS) != 0
        || get(&EXTEND_ZOOMS_MAX) > 0
    {
        GEOMETRY_SCALE.store(0, Ordering::Relaxed);
    } else {
        GEOMETRY_SCALE.store(
            32 - (FULL_DETAIL.load(Ordering::Relaxed) + maxzoom),
            Ordering::Relaxed,
        );
        if get(&GEOMETRY_SCALE) < 0 {
            GEOMETRY_SCALE.store(0, Ordering::Relaxed);
            if !guess_maxzoom {
                // This shouldn't be able to happen any more. Can it still?
                eprintln!(
                    "Full detail + maxzoom > 32, so you are asking for more detail than is available."
                );
            }
        }
    }

    if (basezoom < 0 || droprate < 0.0) && (gamma < 0.0) {
        // Can't use randomized (as opposed to evenly distributed) dot dropping
        // if rate and base aren't known during feature reading.
        gamma = 0.0;
        eprintln!("Forcing -g0 since -B or -r is not known");
    }

    if out_mbtiles.is_none() && out_dir.is_none() {
        eprintln!("{}: must specify -o out.mbtiles or -e directory", argv[0]);
        std::process::exit(EXIT_ARGS);
    }

    if out_mbtiles.is_some() && out_dir.is_some() {
        eprintln!("{}: Options -o and -e cannot be used together", argv[0]);
        std::process::exit(EXIT_ARGS);
    }

    if let Some(out) = &out_mbtiles {
        if force != 0 {
            unlink(out);
        } else if pmtiles_has_suffix(out) {
            check_pmtiles(out, &argv, forcetable != 0);
        }

        outdb = Some(Mutex::new(mbtiles_open(out, &argv, forcetable)));
    }
    if let Some(dir) = &out_dir {
        check_dir(dir, &argv, force != 0, forcetable != 0);
    }

    if additional(A_DROP_BY_ATTRIBUTE_AS_NEEDED) != 0
        && DROP_BY_ATTRIBUTE_AS_NEEDED_ATTRIBUTE
            .read()
            .unwrap()
            .is_empty()
    {
        eprintln!(
            "{}: --drop-by-attribute-as-needed requires an attribute name",
            argv[0]
        );
        std::process::exit(EXIT_ARGS);
    }

    let ret;

    for a in argv.iter().skip(g.optind) {
        let src = Source {
            layer: String::new(),
            file: cstr(a).to_string(),
            ..Default::default()
        };
        sources.push(src);
    }

    if sources.is_empty() {
        let src = Source {
            layer: String::new(),
            file: String::new(), // standard input
            ..Default::default()
        };
        sources.push(src);
    }

    if let Some(layername) = &layername {
        for s in sources.iter_mut() {
            s.layer = layername.clone();
        }
    }

    let mut file_bbox: [i64; 4] = [u32::MAX as i64, u32::MAX as i64, 0, 0];

    let mut file_bbox1: [i64; 4] = [0xFFFFFFFF, 0xFFFFFFFF, 0, 0]; // standard -180 to 180 world plane
    let mut file_bbox2: [i64; 4] = [0x1FFFFFFFF, 0xFFFFFFFF, 0x100000000, 0]; // 0 to 360 world plane

    let fname: String = match (&name, &out_mbtiles) {
        (Some(n), _) => n.clone(),
        (None, Some(o)) => o.clone(),
        (None, None) => out_dir.clone().unwrap_or_default(),
    };

    let input_ret = read_input(
        &mut sources,
        &fname,
        maxzoom,
        minzoom,
        basezoom,
        basezoom_marker_width,
        &mut outdb,
        out_dir.as_deref(),
        &exclude,
        &include,
        exclude_all,
        filter.as_ref(),
        droprate,
        buffer,
        &tmpdir,
        gamma,
        read_parallel,
        forcetable,
        attribution.as_deref(),
        gamma != 0.0,
        &mut file_bbox,
        &mut file_bbox1,
        &mut file_bbox2,
        prefilter.as_deref(),
        postfilter.as_deref(),
        description.as_deref(),
        guess_maxzoom,
        guess_cluster_maxzoom,
        &attribute_types,
        &argv[0],
        &attribute_accum,
        &attribute_descriptions,
        &commandline,
        minimum_maxzoom,
    );

    ret = input_ret.0;

    if let Some(db) = outdb.take() {
        mbtiles_close(db.into_inner().unwrap(), &argv[0]);
    }

    if let Some(out) = &out_mbtiles {
        if pmtiles_has_suffix(out) {
            mbtiles_map_image_to_pmtiles(
                out,
                input_ret.1,
                prevent(P_TILE_COMPRESSION) == 0,
                get(&QUIET) != 0,
                get(&QUIET_PROGRESS) != 0,
            );
        }
    }

    let i = open_fd(get_null_device(), libc::O_RDONLY | O_CLOEXEC, 0);
    // i < files_open_at_start is not an error, because reading from a pipe closes stdin
    if i > files_open_at_start {
        eprintln!("Internal error: did not close all files: {}", i);
        std::process::exit(EXIT_IMPOSSIBLE);
    }

    drop(filter);

    std::process::exit(ret);
}

/// libc `mkstemp`. `name` is a template ending in XXXXXX, which is replaced
/// with the name of the file created. Returns the file descriptor, or -1.
pub fn mkstemp(name: &mut String) -> i32 {
    crate::platform::mkstemp(name)
}

/// `mkstemp_cloexec`. `name` is a template ending in XXXXXX, which is replaced
/// with the name of the file created. Returns the file descriptor, or -1.
pub fn mkstemp_cloexec(name: &mut String) -> i32 {
    let fd = mkstemp(name);
    // On Windows, a child never inherits it.
    #[cfg(unix)]
    if fd >= 0 {
        // SAFETY: fcntl on the descriptor just created.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            perror("cloexec for temporary file");
            std::process::exit(EXIT_OPEN);
        }
    }
    fd
}

/// `fopen_oflag`: opens `name` with `oflag` and gives the file, which the
/// caller buffers as `mode` would. `None` on failure, with errno set.
pub fn fopen_oflag(name: &str, _mode: &str, oflag: i32) -> Option<File> {
    let fd = open_fd(name, oflag, 0);
    if fd < 0 {
        return None;
    }
    // SAFETY: the File takes over the descriptor just opened.
    Some(unsafe { crate::platform::file_from_fd(fd) })
}

/// `progress_time()`: whether enough time has passed to print progress again.
pub fn progress_time() -> bool {
    // tile-join and tippecanoe-decode link a progress_time() that is always false
    if !get(&PROGRESS_TIME_ENABLED) {
        return false;
    }

    let progress_interval = PROGRESS_INTERVAL.load();
    if progress_interval == 0.0 {
        return true;
    }

    let now: f64 = match crate::platform::time_of_day() {
        None => {
            eprintln!("{}: Can't get the time of day: {}", av(), strerror(errno()));
            0.0
        }
        Some(now) => now,
    };

    if now - LAST_PROGRESS.load() >= progress_interval {
        LAST_PROGRESS.store(now);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_layout() {
        let mut ix = Index {
            start: 123,
            end: 456,
            ix: 0xfedcba9876543210,
            ..Default::default()
        };
        ix.set_segment(-2);
        ix.set_t(3);
        ix.set_seq((1u64 << 46) - 1);
        assert_eq!(ix.segment(), -2);
        assert_eq!(ix.t(), 3);
        assert_eq!(ix.seq(), (1u64 << 46) - 1);
        ix.set_seq(5);
        ix.set_t(1);
        assert_eq!(ix.segment(), -2);
        assert_eq!(ix.t(), 1);
        assert_eq!(ix.seq(), 5);

        let b = ix.to_bytes();
        assert_eq!(Index::from_bytes(&b), ix);
        // The C++ bitfields: segment in the low 16 bits, then t, then seq
        assert_eq!(
            u64::from_ne_bytes(b[24..32].try_into().unwrap()),
            (5 << 18) | (1 << 16) | 0xFFFE
        );
    }

    #[test]
    fn index_ordering() {
        let mut a = Index {
            ix: 5,
            ..Default::default()
        };
        a.set_seq(10);
        let mut b = Index {
            ix: 5,
            ..Default::default()
        };
        b.set_seq(11);
        let c = Index {
            ix: 4,
            ..Default::default()
        };
        assert_eq!(indexcmp(&a, &b), -1);
        assert_eq!(indexcmp(&b, &a), 1);
        assert_eq!(indexcmp(&c, &a), -1);
        assert_eq!(indexcmp(&a, &a), 0);
    }

    #[test]
    fn merging_sorted_runs() {
        // Three sorted runs of the index, merged as merge() does it
        let mut map: Vec<u8> = Vec::new();
        let runs: [&[u64]; 3] = [&[1, 4, 9], &[2, 3, 10], &[5]];
        let mut merges = Vec::new();
        for run in runs.iter() {
            let start = map.len() as i64;
            for &v in run.iter() {
                let mut ix = Index {
                    ix: v,
                    ..Default::default()
                };
                ix.set_seq(v);
                map.extend_from_slice(&ix.to_bytes());
            }
            merges.push(Mergelist {
                start,
                end: map.len() as i64,
                next: None,
            });
        }

        let mut head = None;
        for i in 0..merges.len() {
            insert(&mut merges, i, &mut head, &map);
        }
        let mut out = Vec::new();
        while let Some(h) = head {
            out.push(index_at(&map, merges[h].start).ix);
            merges[h].start += INDEX_SIZE as i64;
            head = merges[h].next;
            merges[h].next = None;
            if merges[h].start < merges[h].end {
                insert(&mut merges, h, &mut head, &map);
            }
        }
        assert_eq!(out, vec![1, 2, 3, 4, 5, 9, 10]);
    }

    #[test]
    fn drop_states() {
        let mut ds = vec![DropState::default(); 5];
        prep_drop_states(&mut ds, 4, 3, 2.5);
        assert_eq!(ds[3].interval, 0.0);
        assert_eq!(ds[4].interval, 0.0);
        assert_eq!(ds[2].interval, 2.5f64.ln().exp());
        assert_eq!(ds[0].interval, (2.5f64.ln() * 3.0).exp());

        assert_eq!(round_droprate(2.123456789), 2.12346);
    }

    #[test]
    fn vertex_ordering() {
        let a = Vertex::new(
            Draw::new(VT_MOVETO, 1, 1),
            Draw::new(VT_MOVETO, 5, 5),
            Draw::new(VT_MOVETO, 9, 9),
        );
        let b = Vertex::new(
            Draw::new(VT_MOVETO, 1, 2),
            Draw::new(VT_MOVETO, 5, 5),
            Draw::new(VT_MOVETO, 9, 9),
        );
        assert_eq!(vertexcmp(&a, &b), -1);
        assert_eq!(vertexcmp_bytes(&b.to_bytes(), &a.to_bytes()), 1);
        assert_eq!(
            nodecmp_bytes(&Node { index: 3 }.to_bytes(), &Node { index: 3 }.to_bytes()),
            0
        );
    }
}
