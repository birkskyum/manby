//! Port of plugin.hpp and plugin.cpp: running prefilters and postfilters as
//! child processes that read and write GeoJSON.
//!
//! The C++ functions take the whole `layermaps` and `layer_unmaps` vectors and
//! a `tiling_seg` index, and only ever touch the entries at `tiling_seg`. Here
//! they take those two entries, which belong to the calling tiling thread.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::os::fd::{FromRawFd, IntoRawFd};
use std::os::raw::c_char;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::clip::fix_polygon;
use crate::cstd::{perror, strerror};
use crate::errors::{EXIT_CLOSE, EXIT_IMPOSSIBLE, EXIT_JSON, EXIT_OPEN, EXIT_PTHREAD};
use crate::geometry::{DrawVec, VT_LINETO, VT_MOVETO, VT_POLYGON};
use crate::globals::{GEOMETRY_SCALE, get};
use crate::jsonpull::{JsonPull, JsonType};
use crate::mbtiles::{LayermapEntry, add_to_tilestats};
use crate::mvt::{
    MVT_LINESTRING, MVT_NULL, MVT_POINT, MVT_POLYGON, MvtLayer, mvt_value_to_serial_val,
};
use crate::read_json::{
    GEOM_TYPES, GEOMETRY_NAMES, MB_GEOMETRY, json_context, parse_coordinates, stringify_value,
};
use crate::serial::{FEATURE_DROPPED, FEATURE_KEPT, KeyPool, SerialFeature};
use crate::write_json::{JsonWriter, layer_to_geojson};

/// A `FILE *` from `fdopen(fd, "w")` on the pipe to a filter. Writes are
/// buffered, and as with stdio a failed write is only reported by `fclose`,
/// so that a filter that stops reading does not interrupt the writer partway.
pub struct PipeWriter {
    w: BufWriter<File>,
    err: Option<std::io::Error>,
}

impl PipeWriter {
    /// `fdopen(fd, "w")`. Takes ownership of the descriptor.
    pub fn fdopen(fd: i32) -> PipeWriter {
        // SAFETY: the caller hands over an open descriptor that nothing else closes.
        let f = unsafe { File::from_raw_fd(fd) };
        PipeWriter {
            w: BufWriter::new(f),
            err: None,
        }
    }

    /// `fclose`: flushes and closes, returning the first error since `fdopen`.
    pub fn fclose(mut self) -> Result<(), std::io::Error> {
        if self.err.is_none()
            && let Err(e) = self.w.flush()
        {
            self.err = Some(e);
        }
        if let Some(e) = self.err.take() {
            return Err(e);
        }
        let f = match self.w.into_inner() {
            Ok(f) => f,
            Err(e) => return Err(e.into_error()),
        };
        // SAFETY: the descriptor came from into_raw_fd and is closed exactly once.
        if unsafe { libc::close(f.into_raw_fd()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Write for PipeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.err.is_none()
            && let Err(e) = self.w.write_all(buf)
        {
            self.err = Some(e);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.err.is_none()
            && let Err(e) = self.w.flush()
        {
            self.err = Some(e);
        }
        Ok(())
    }
}

/// Whether the error from `fclose` was `EPIPE`.
pub fn is_epipe(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(libc::EPIPE)
}

/// `perror(s)` for an error held in a `std::io::Error` rather than in `errno`.
pub fn perror_io(s: &str, e: &std::io::Error) {
    match e.raw_os_error() {
        Some(code) => eprintln!("{}: {}", s, strerror(code)),
        None => eprintln!("{}: {}", s, e),
    }
}

struct WriterArg<'a> {
    write_to: i32,
    layers: &'a [MvtLayer],
    z: u32,
    x: u32,
    y: u32,
    extent: i32,
}

fn run_writer(wa: &WriterArg<'_>) {
    let mut fp = PipeWriter::fdopen(wa.write_to);

    {
        let mut state = JsonWriter::new_file(&mut fp);
        for layer in wa.layers {
            layer_to_geojson(
                layer,
                wa.z,
                wa.x,
                wa.y,
                false,
                true,
                false,
                true,
                0,
                0,
                0,
                true,
                &mut state,
                0.0,
                &BTreeSet::new(),
            );
        }
    }

    if let Err(e) = fp.fclose() {
        if is_epipe(&e) {
            static WARNED: AtomicBool = AtomicBool::new(false);
            if !WARNED.load(Ordering::Relaxed) {
                eprintln!("Warning: broken pipe in postfilter");
                WARNED.store(true, Ordering::Relaxed);
            }
        } else {
            perror_io("fclose output to filter", &e);
            std::process::exit(EXIT_CLOSE);
        }
    }
}

/// Adds `layername` to the tiling thread's layer map if it is not there yet,
/// as `parse_layers` and `parse_feature` both do.
fn add_layer(
    layermap: &mut BTreeMap<String, LayermapEntry>,
    layer_unmap: &mut Vec<String>,
    layername: &str,
    z: i32,
) {
    if !layermap.contains_key(layername) {
        let mut lme = LayermapEntry::new(layermap.len());
        lme.minzoom = z;
        lme.maxzoom = z;
        let id = lme.id;

        layermap.insert(layername.to_string(), lme);

        if id >= layer_unmap.len() {
            layer_unmap.resize(id + 1, String::new());
            layer_unmap[id] = layername.to_string();
        }
    }
}

/// Reads from the postfilter
#[allow(clippy::too_many_arguments)]
fn parse_layers(
    fd: i32,
    z: i32,
    x: u32,
    y: u32,
    layermap: &mut BTreeMap<String, LayermapEntry>,
    tiling_seg: usize,
    layer_unmap: &mut Vec<String>,
    extent: i32,
) -> Vec<MvtLayer> {
    // SAFETY: fd is the read end of the filter's output pipe, owned here.
    let mut f = unsafe { File::from_raw_fd(fd) };

    let out = crate::read_json::parse_layers(
        Box::new(BufReader::new(&mut f)),
        z,
        x,
        y,
        extent,
        false,
    );

    // SAFETY: the descriptor came from into_raw_fd and is closed exactly once.
    if unsafe { libc::close(f.into_raw_fd()) } != 0 {
        perror("fclose postfilter output");
        std::process::exit(EXIT_CLOSE);
    }

    let _ = tiling_seg;
    for layer in &out {
        let layername = &layer.name;

        add_layer(layermap, layer_unmap, layername, z);

        let ts = match layermap.get_mut(layername) {
            Some(ts) => ts,
            None => {
                eprintln!("Internal error: layer {} not found", layername);
                std::process::exit(EXIT_IMPOSSIBLE);
            }
        };
        if z < ts.minzoom {
            ts.minzoom = z;
        }
        if z > ts.maxzoom {
            ts.maxzoom = z;
        }

        for feature in &layer.features {
            if feature.type_ == MVT_POINT {
                ts.points += 1;
            } else if feature.type_ == MVT_LINESTRING {
                ts.lines += 1;
            } else if feature.type_ == MVT_POLYGON {
                ts.polygons += 1;
            }

            let mut i = 0;
            while i + 1 < feature.tags.len() {
                let key = &layer.keys[feature.tags[i] as usize];
                let val = &layer.values[feature.tags[i + 1] as usize];

                // Nulls can be excluded here because this is the postfilter
                // and it is nearly time to create the vector representation

                if val.type_ != MVT_NULL {
                    add_to_tilestats(&mut ts.tilestats, key, &mvt_value_to_serial_val(val));
                }
                i += 2;
            }
        }
    }

    out
}

/// Reads from the prefilter
#[allow(clippy::too_many_arguments)]
pub fn parse_feature(
    jp: &mut JsonPull<'_>,
    z: i32,
    x: u32,
    y: u32,
    layermap: &mut BTreeMap<String, LayermapEntry>,
    tiling_seg: usize,
    layer_unmap: &mut Vec<String>,
    postfilter: bool,
    key_pool: &mut KeyPool,
) -> SerialFeature {
    let mut sf = SerialFeature::default();

    loop {
        // json_read also returns the intermediate containers of a feature
        // still being assembled, so the `continue` paths leave `j` alone;
        // freeing one would splice it out of its parent. `j` is only freed
        // once it is a complete Feature.
        let id = match jp.read() {
            Some(id) => id,
            None => {
                if let Some(error) = jp.error {
                    eprint!("Filter output:{}: {}: ", jp.line, error);
                    match jp.root() {
                        Some(root) => json_context(root),
                        None => eprintln!(),
                    }
                    std::process::exit(EXIT_JSON);
                }

                jp.clear_root();
                sf.t = -1;
                return sf;
            }
        };

        let found = {
            let j = jp.get(id);

            let type_ = match j.hash_get("type") {
                Some(t) if t.type_() == JsonType::String => t,
                _ => continue,
            };
            if type_.string() != "Feature" {
                continue;
            }

            let geometry = match j.hash_get("geometry") {
                Some(g) => g,
                None => {
                    eprint!("Filter output:{}: filtered feature with no geometry: ", jp.line);
                    json_context(j);
                    std::process::exit(EXIT_JSON);
                }
            };

            let properties = match j.hash_get("properties") {
                Some(p) if p.type_() == JsonType::Hash || p.type_() == JsonType::Null => p,
                _ => {
                    eprint!("Filter output:{}: feature without properties hash: ", jp.line);
                    json_context(j);
                    std::process::exit(EXIT_JSON);
                }
            };

            let geometry_type = match geometry.hash_get("type") {
                Some(g) => g,
                None => {
                    eprint!(
                        "Filter output:{}: null geometry (additional not reported): ",
                        jp.line
                    );
                    json_context(j);
                    std::process::exit(EXIT_JSON);
                }
            };

            if geometry_type.type_() != JsonType::String {
                eprint!("Filter output:{}: geometry type is not a string: ", jp.line);
                json_context(j);
                std::process::exit(EXIT_JSON);
            }

            let coordinates = match geometry.hash_get("coordinates") {
                Some(c) if c.type_() == JsonType::Array => c,
                _ => {
                    eprint!("Filter output:{}: feature without coordinates array: ", jp.line);
                    json_context(j);
                    std::process::exit(EXIT_JSON);
                }
            };

            let mut t = 0;
            while t < GEOM_TYPES {
                if geometry_type.string() == GEOMETRY_NAMES[t] {
                    break;
                }
                t += 1;
            }
            if t >= GEOM_TYPES {
                eprint!(
                    "Filter output:{}: Can't handle geometry type {}: ",
                    jp.line,
                    geometry_type.string()
                );
                json_context(j);
                std::process::exit(EXIT_JSON);
            }

            let mut dv = DrawVec::new();
            parse_coordinates(
                t as i32,
                coordinates,
                &mut dv,
                VT_MOVETO,
                "Filter output",
                jp.line,
                j,
            );
            if MB_GEOMETRY[t] == VT_POLYGON {
                dv = fix_polygon(&dv, false, false);
            }

            // Scale and offset geometry from global to tile
            let scale = (1i64 << get(&GEOMETRY_SCALE)) as f64;
            for d in dv.iter_mut() {
                let mut sx: u32 = 0;
                let mut sy: u32 = 0;
                if z != 0 {
                    sx = x << (32 - z);
                    sy = y << (32 - z);
                }
                // PORT: clang fuses the multiply and subtract here.
                d.x = (d.x as f64 / scale).round().mul_add(scale, -(sx as f64)) as i64;
                d.y = (d.y as f64 / scale).round().mul_add(scale, -(sy as f64)) as i64;
            }

            if !dv.is_empty() {
                sf.t = MB_GEOMETRY[t] as i8;
                sf.segment = tiling_seg as i32;
                sf.geometry = dv;
                sf.seq = 0;
                sf.index = 0;
                sf.bbox[0] = i64::MAX;
                sf.bbox[1] = i64::MAX;
                sf.bbox[2] = i64::MIN;
                sf.bbox[3] = i64::MIN;
                sf.extent = 0;
                sf.has_id = false;

                let mut layername = "unknown".to_string();
                if let Some(tippecanoe) = j.hash_get("tippecanoe") {
                    if let Some(layer) = tippecanoe.hash_get("layer")
                        && layer.type_() == JsonType::String
                    {
                        layername = layer.string().to_string();
                    }

                    if let Some(index) = tippecanoe.hash_get("index")
                        && index.type_() == JsonType::Number
                    {
                        sf.index = index.number() as u64;
                    }

                    if let Some(sequence) = tippecanoe.hash_get("sequence")
                        && sequence.type_() == JsonType::Number
                    {
                        sf.seq = sequence.number() as i64;
                    }

                    if let Some(extent) = tippecanoe.hash_get("extent")
                        && extent.type_() == JsonType::Number
                    {
                        sf.extent = extent.number() as i64;
                    }

                    match tippecanoe.hash_get("dropped") {
                        Some(dropped) if dropped.type_() == JsonType::True => {
                            sf.dropped = FEATURE_DROPPED; // dropped
                        }
                        _ => {
                            sf.dropped = FEATURE_KEPT; // kept
                        }
                    }
                }

                for d in &sf.geometry {
                    if d.op == VT_MOVETO || d.op == VT_LINETO {
                        if d.x < sf.bbox[0] {
                            sf.bbox[0] = d.x;
                        }
                        if d.y < sf.bbox[1] {
                            sf.bbox[1] = d.y;
                        }
                        if d.x > sf.bbox[2] {
                            sf.bbox[2] = d.x;
                        }
                        if d.y > sf.bbox[3] {
                            sf.bbox[3] = d.y;
                        }
                    }
                }

                if let Some(id) = j.hash_get("id")
                    && id.type_() == JsonType::Number
                {
                    sf.id = id.number() as u64;
                    if id.large_unsigned() > 0 {
                        sf.id = id.large_unsigned();
                    }
                    sf.has_id = true;
                }

                add_layer(layermap, layer_unmap, &layername, z);

                let ts = match layermap.get_mut(&layername) {
                    Some(ts) => ts,
                    None => {
                        eprintln!("Internal error: layer {} not found", layername);
                        std::process::exit(EXIT_IMPOSSIBLE);
                    }
                };
                sf.layer = ts.id as i64;

                if z < ts.minzoom {
                    ts.minzoom = z;
                }
                if z > ts.maxzoom {
                    ts.maxzoom = z;
                }

                if !postfilter {
                    if sf.t as i32 == MVT_POINT {
                        ts.points += 1;
                    } else if sf.t as i32 == MVT_LINESTRING {
                        ts.lines += 1;
                    } else if sf.t as i32 == MVT_POLYGON {
                        ts.polygons += 1;
                    }
                }

                if properties.type_() == JsonType::Hash {
                    for (key, value) in properties.entries() {
                        let v = stringify_value(Some(value), "Filter output", jp.line, j);

                        // Nulls can be excluded here because the expression evaluation filter
                        // would have already run before prefiltering

                        if v.type_ != MVT_NULL {
                            sf.full_keys.push(key_pool.pool(key.string()));

                            if !postfilter {
                                add_to_tilestats(&mut ts.tilestats, key.string(), &v);
                            }

                            sf.full_values.push(v);
                        }
                    }
                }

                true
            } else {
                false
            }
        };

        jp.free(id);
        if found {
            return sf;
        }
    }
}

static PIPE_LOCK: Mutex<()> = Mutex::new(());

fn cstring(s: &str) -> CString {
    match CString::new(s) {
        Ok(c) => c,
        Err(_) => {
            eprintln!("{}: filter contains a NUL", s);
            std::process::exit(EXIT_PTHREAD);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn setup_filter(
    filter: &str,
    write_to: &mut i32,
    read_from: &mut i32,
    pid: &mut libc::pid_t,
    z: u32,
    x: u32,
    y: u32,
) {
    // This will create two pipes, a new thread, and a new process.
    //
    // The new process will read from one pipe and write to the other, and execute the filter.
    // The new thread will write the GeoJSON to the pipe that leads to the filter.
    // The original thread will read the GeoJSON from the filter and convert it back into vector tiles.

    let guard = match PIPE_LOCK.lock() {
        Ok(g) => g,
        Err(_) => {
            perror("pthread_mutex_lock (pipe)");
            std::process::exit(EXIT_PTHREAD);
        }
    };

    let mut pipe_orig = [0i32; 2];
    let mut pipe_filtered = [0i32; 2];
    // SAFETY: pipe_orig has room for the two descriptors.
    if unsafe { libc::pipe(pipe_orig.as_mut_ptr()) } < 0 {
        perror("pipe (original features)");
        std::process::exit(EXIT_OPEN);
    }
    // SAFETY: pipe_filtered has room for the two descriptors.
    if unsafe { libc::pipe(pipe_filtered.as_mut_ptr()) } < 0 {
        perror("pipe (filtered features)");
        std::process::exit(EXIT_OPEN);
    }

    let z_str = cstring(&z.to_string());
    let x_str = cstring(&x.to_string());
    let y_str = cstring(&y.to_string());
    let filter_str = cstring(filter);
    let argv: [*const c_char; 8] = [
        c"sh".as_ptr(),
        c"-c".as_ptr(),
        filter_str.as_ptr(),
        c"sh".as_ptr(),
        z_str.as_ptr(),
        x_str.as_ptr(),
        y_str.as_ptr(),
        std::ptr::null(),
    ];

    // SAFETY: the child only calls async-signal-safe functions until it execs
    // or exits, and everything it needs was allocated before the fork.
    *pid = unsafe { libc::fork() };
    if *pid < 0 {
        perror("fork");
        std::process::exit(EXIT_PTHREAD);
    } else if *pid == 0 {
        // child

        // SAFETY: plain system calls on descriptors this process owns, then exec.
        unsafe {
            if libc::dup2(pipe_orig[0], 0) < 0 {
                libc::perror(c"dup child stdin".as_ptr());
                libc::_exit(EXIT_OPEN);
            }
            if libc::dup2(pipe_filtered[1], 1) < 0 {
                libc::perror(c"dup child stdout".as_ptr());
                libc::_exit(EXIT_OPEN);
            }
            if libc::close(pipe_orig[1]) != 0 {
                libc::perror(c"close output to filter".as_ptr());
                libc::_exit(EXIT_CLOSE);
            }
            if libc::close(pipe_filtered[0]) != 0 {
                libc::perror(c"close input from filter".as_ptr());
                libc::_exit(EXIT_CLOSE);
            }
            if libc::close(pipe_orig[0]) != 0 {
                libc::perror(c"close dup input of filter".as_ptr());
                libc::_exit(EXIT_CLOSE);
            }
            if libc::close(pipe_filtered[1]) != 0 {
                libc::perror(c"close dup output of filter".as_ptr());
                libc::_exit(EXIT_CLOSE);
            }

            // XXX close other fds?

            libc::execvp(c"sh".as_ptr(), argv.as_ptr());
            libc::perror(c"exec".as_ptr());
            libc::_exit(EXIT_PTHREAD);
        }
    } else {
        // parent

        // SAFETY: closing and flagging descriptors this process owns.
        unsafe {
            if libc::close(pipe_orig[0]) != 0 {
                perror("close filter-side reader");
                std::process::exit(EXIT_CLOSE);
            }
            if libc::close(pipe_filtered[1]) != 0 {
                perror("close filter-side writer");
                std::process::exit(EXIT_CLOSE);
            }
            if libc::fcntl(pipe_orig[1], libc::F_SETFD, libc::FD_CLOEXEC) != 0 {
                perror("cloxec output to filter");
                std::process::exit(EXIT_CLOSE);
            }
            if libc::fcntl(pipe_filtered[0], libc::F_SETFD, libc::FD_CLOEXEC) != 0 {
                perror("cloxec input from filter");
                std::process::exit(EXIT_CLOSE);
            }
        }

        drop(guard);

        *write_to = pipe_orig[1];
        *read_from = pipe_filtered[0];
    }
}

/// `waitpid` until the filter process has exited or been killed.
pub fn wait_for_filter(pid: libc::pid_t, what: &str) {
    loop {
        let mut stat_loc: i32 = 0;
        // SAFETY: stat_loc is a valid place for the status.
        if unsafe { libc::waitpid(pid, &mut stat_loc, 0) } < 0 {
            perror(what);
            std::process::exit(EXIT_PTHREAD);
        }
        if libc::WIFEXITED(stat_loc) || libc::WIFSIGNALED(stat_loc) {
            break;
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn filter_layers(
    filter: &str,
    layers: &[MvtLayer],
    z: u32,
    x: u32,
    y: u32,
    layermap: &mut BTreeMap<String, LayermapEntry>,
    tiling_seg: usize,
    layer_unmap: &mut Vec<String>,
    extent: i32,
) -> Vec<MvtLayer> {
    let mut write_to = -1;
    let mut read_from = -1;
    let mut pid: libc::pid_t = 0;
    setup_filter(filter, &mut write_to, &mut read_from, &mut pid, z, x, y);

    let wa = WriterArg {
        write_to,
        layers,
        z,
        x,
        y,
        extent,
    };

    std::thread::scope(|scope| {
        // this does need to be a real thread, so we can pipe both to and from it
        let writer = match std::thread::Builder::new().spawn_scoped(scope, || run_writer(&wa)) {
            Ok(w) => w,
            Err(_) => {
                perror("pthread_create (filter writer)");
                std::process::exit(EXIT_PTHREAD);
            }
        };

        let nlayers = parse_layers(
            read_from,
            z as i32,
            x,
            y,
            layermap,
            tiling_seg,
            layer_unmap,
            extent,
        );

        wait_for_filter(pid, "waitpid for filter\n");

        if let Err(e) = writer.join() {
            std::panic::resume_unwind(e);
        }

        nlayers
    })
}
