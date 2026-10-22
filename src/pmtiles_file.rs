//! Port of pmtiles_file.hpp and pmtiles_file.cpp: PMTiles output and input.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::{c_double, c_int};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::process::exit;

use rusqlite::Connection;
use rusqlite::types::{ToSqlOutput, ValueRef};

use crate::errors::{EXIT_EXISTS, EXIT_JSON, EXIT_OPEN, EXIT_SQLITE};
use crate::jsonpull::{JsonPull, JsonType};
use crate::mbtiles::{
    Metadata, c_str, c_str_bytes, sqlite3_errmsg, sqlite3_exec, sqlite3_mprintf, sqlite3_open,
};
use crate::mvt::{compress, decompress};
use crate::pmtiles::{self, EntryV3, EntryZxy};
use crate::write_json::JsonWriter;

pub fn pmtiles_has_suffix(filename: &str) -> bool {
    let lenstr = filename.len();
    if lenstr < 8 {
        return false;
    }
    if &filename.as_bytes()[lenstr - 8..] == b".pmtiles" {
        return true;
    }
    false
}

pub fn check_pmtiles(filename: &str, argv: &[String], forcetable: bool) {
    if fs::metadata(filename).is_ok() {
        eprintln!(
            "{}: Tileset \"{}\" already exists. You can use --force if you want to delete the old tileset.",
            argv[0], filename
        );
        eprintln!("{}: {}: file exists", argv[0], filename);

        if forcetable {
            eprintln!("{}: --allow-existing is not supported for pmtiles", argv[0]);
        }

        exit(EXIT_EXISTS);
    }
}

fn decompress_fn(input: &[u8], compression: u8) -> Vec<u8> {
    let mut output: Vec<u8> = Vec::new();
    if compression == pmtiles::COMPRESSION_NONE {
        output = input.to_vec();
    } else if compression == pmtiles::COMPRESSION_GZIP {
        decompress(input, &mut output);
    } else {
        pmtiles::throw_uncaught("std::runtime_error", "Unknown or unsupported compression.");
    }

    output
}

fn compress_fn(input: &[u8], compression: u8) -> Vec<u8> {
    let mut output: Vec<u8> = Vec::new();
    if compression == pmtiles::COMPRESSION_NONE {
        output = input.to_vec();
    } else if compression == pmtiles::COMPRESSION_GZIP {
        compress(input, &mut output, true);
    } else {
        pmtiles::throw_uncaught("std::runtime_error", "Unknown or unsupported compression.");
    }

    output
}

/// `pmtiles_map` is the whole archive, memory-mapped.
pub fn pmtiles_entries_tms(pmtiles_map: &[u8], minzoom: i32, maxzoom: i32) -> Vec<EntryZxy> {
    let all_entries = pmtiles::entries_tms(&decompress_fn, pmtiles_map);
    all_entries
        .into_iter()
        .filter(|e| e.z as i32 >= minzoom && e.z as i32 <= maxzoom)
        .collect()
}

/// Returns the offset and length of the tile in the archive, or (0, 0).
pub fn pmtiles_get_tile(pmtiles_map: &[u8], z: i32, x: i32, y: i32) -> (u64, u32) {
    pmtiles::get_tile(&decompress_fn, pmtiles_map, z as u8, x as u32, y as u32)
}

/// `std::to_string(double)`, which formats like `%f`.
fn to_string_double(d: f64) -> String {
    if d.is_nan() {
        return "nan".to_string();
    }
    format!("{:.6}", d)
}

fn out(state: &mut JsonWriter<'_>, k: &str, v: &str) {
    state.json_comma_newline();
    state.json_write_string(k);
    state.json_write_string(v);
}

fn metadata_to_pmtiles_json(m: &Metadata) -> Vec<u8> {
    let mut buf = String::new();
    {
        let mut state = JsonWriter::new_string(&mut buf);

        state.json_write_hash();
        state.json_write_newline();

        out(&mut state, "name", &m.name);
        out(&mut state, "format", &m.format);
        out(&mut state, "type", &m.type_);
        out(&mut state, "description", &m.description);
        out(&mut state, "version", &m.version.to_string());
        if !m.attribution.is_empty() {
            out(&mut state, "attribution", &m.attribution);
        }
        if !m.strategies_json.is_empty() {
            state.json_comma_newline();
            state.json_write_string("strategies");
            state.json_write_json(&m.strategies_json);
        }

        if !m.decisions_json.is_empty() {
            state.json_comma_newline();
            state.json_write_string("tippecanoe_decisions");
            state.json_write_json(&m.decisions_json);
        }

        out(&mut state, "generator", &m.generator);
        out(&mut state, "generator_options", &m.generator_options);

        let bounds2 = to_string_double(m.minlon2)
            + ","
            + &to_string_double(m.minlat2)
            + ","
            + &to_string_double(m.maxlon2)
            + ","
            + &to_string_double(m.maxlat2);
        out(&mut state, "antimeridian_adjusted_bounds", &bounds2);

        if !m.vector_layers_json.is_empty() {
            state.json_comma_newline();
            state.json_write_string("vector_layers");
            state.json_write_json(&m.vector_layers_json);
        }

        if !m.tilestats_json.is_empty() {
            state.json_comma_newline();
            state.json_write_string("tilestats");
            state.json_write_json(&m.tilestats_json);
        }

        state.json_write_newline();
        state.json_end_hash();
        state.json_write_newline();
    }
    let mut compressed: Vec<u8> = Vec::new();
    compress(buf.as_bytes(), &mut compressed, true);
    compressed
}

/// The bytes of a column, converted the way `sqlite3_column_blob` and
/// `sqlite3_column_text` convert numbers.
fn column_bytes<'a>(v: ValueRef<'a>) -> Cow<'a, [u8]> {
    match v {
        ValueRef::Null => Cow::Borrowed(&[]),
        ValueRef::Integer(i) => Cow::Owned(i.to_string().into_bytes()),
        ValueRef::Real(f) => {
            let s = sqlite3_mprintf!(c"%!.15g", f as c_double);
            Cow::Owned(s.into_bytes())
        }
        ValueRef::Text(t) => Cow::Borrowed(t),
        ValueRef::Blob(b) => Cow::Borrowed(b),
    }
}

/// `sqlite3_column_int`.
fn column_int(v: ValueRef<'_>) -> i32 {
    match v {
        ValueRef::Null => 0,
        ValueRef::Integer(i) => i as i32,
        ValueRef::Real(f) => f as i64 as i32,
        ValueRef::Text(t) | ValueRef::Blob(t) => {
            crate::cstd::atoll(&String::from_utf8_lossy(t)) as i32
        }
    }
}

pub fn mbtiles_map_image_to_pmtiles(
    fname: &str,
    m: Metadata,
    tile_compression: bool,
    fquiet: bool,
    fquiet_progress: bool,
) {
    let db = match sqlite3_open(fname) {
        Ok(db) => db,
        Err(msg) => {
            eprintln!("{}: {}", fname, msg);
            exit(EXIT_SQLITE);
        }
    };

    if let Err(err) = sqlite3_exec(&db, c"PRAGMA integrity_check;") {
        eprintln!("{}: integrity_check: {}", fname, err);
        exit(EXIT_SQLITE);
    }

    // materialize list of all tile IDs
    let mut tile_ids: Vec<u64> = Vec::new();

    {
        let sql = "SELECT zoom_level, tile_column, tile_row FROM map";

        let mut stmt = match db.prepare(sql) {
            Ok(stmt) => stmt,
            Err(_) => {
                eprintln!("{}: select failed: {}", fname, sqlite3_errmsg(&db));
                exit(EXIT_SQLITE);
            }
        };

        let mut rows = stmt.raw_query();
        while let Ok(Some(row)) = rows.next() {
            let zoom = column_int(row.get_ref(0).unwrap());
            let x = column_int(row.get_ref(1).unwrap());
            let sorty = column_int(row.get_ref(2).unwrap());
            let y = 1i64
                .wrapping_shl(zoom as u32)
                .wrapping_sub(1)
                .wrapping_sub(sorty as i64) as i32;
            let res = pmtiles::zxy_to_tileid(zoom as u8, x as u32, y as u32);
            tile_ids.push(res);
        }
    }

    tile_ids.sort();

    let mut hash_to_offset_len: HashMap<Vec<u8>, (u64, u64)> = HashMap::new();
    let mut entries: Vec<EntryV3> = Vec::new();
    let mut offset: u64 = 0;

    let tmpname = fname.to_string() + ".tmp";

    // write tile data to tempfile in clustered order
    {
        let map_sql =
            "SELECT tile_id FROM map WHERE zoom_level = ? AND tile_column = ? AND tile_row = ?";
        let mut map_stmt = match db.prepare(map_sql) {
            Ok(stmt) => stmt,
            Err(_) => {
                eprintln!("{}: select failed: {}", fname, sqlite3_errmsg(&db));
                exit(EXIT_SQLITE);
            }
        };

        let image_sql = "SELECT tile_data FROM images WHERE tile_id = ?";
        let mut image_stmt = match db.prepare(image_sql) {
            Ok(stmt) => stmt,
            Err(_) => {
                eprintln!("{}: select failed: {}", fname, sqlite3_errmsg(&db));
                exit(EXIT_SQLITE);
            }
        };

        // An std::ofstream that fails to open drops what is written to it.
        let mut tmp_ostream = File::create(&tmpname).ok().map(BufWriter::new);

        let mut idx: i32 = 0;
        let mut progress_reported: i32 = -1;
        for &tile_id in &tile_ids {
            idx += 1;
            let progress = (idx as f64 / tile_ids.len() as f64) * 100.0;
            let zxy = pmtiles::tileid_to_zxy(tile_id);
            if !fquiet
                && !fquiet_progress
                && crate::tippecanoe_main::progress_time()
                && progress as i32 != progress_reported
            {
                eprint!("  {:3.1}%  {}/{}/{}  \r", progress, zxy.z, zxy.x, zxy.y);
                progress_reported = progress as i32;
            }
            let _ = map_stmt.raw_bind_parameter(1, zxy.z as i32);
            let _ = map_stmt.raw_bind_parameter(2, zxy.x as i32);
            let _ = map_stmt.raw_bind_parameter(3, ((1i64 << zxy.z) - 1 - zxy.y as i64) as i32);

            let hsh: Vec<u8> = {
                let mut rows = map_stmt.raw_query();
                match rows.next() {
                    Ok(Some(row)) => {
                        let text = column_bytes(row.get_ref(0).unwrap());
                        // PORT: the C++ makes a std::string from the C string,
                        // so the hash ends at a NUL. A NULL tile_id is undefined
                        // behaviour there and an empty hash here.
                        let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
                        text[..end].to_vec()
                    }
                    _ => {
                        eprintln!("Corrupt mbtiles file: null entry in map table");
                        exit(EXIT_SQLITE);
                    }
                }
            };

            if let Some(&offset_len) = hash_to_offset_len.get(&hsh) {
                if !entries.is_empty()
                    && tile_id == entries[entries.len() - 1].tile_id + 1
                    && entries[entries.len() - 1].offset == offset_len.0
                {
                    let last = entries.len() - 1;
                    entries[last].run_length += 1;
                } else {
                    entries.push(EntryV3::new(tile_id, offset_len.0, offset_len.1 as u32, 1));
                }
            } else {
                let _ =
                    image_stmt.raw_bind_parameter(1, ToSqlOutput::Borrowed(ValueRef::Text(&hsh)));
                let mut rows = image_stmt.raw_query();
                let row = match rows.next() {
                    Ok(Some(row)) => row,
                    _ => {
                        eprintln!("Corrupt mbtiles file: null entry in image table");
                        exit(EXIT_SQLITE);
                    }
                };

                let blob = column_bytes(row.get_ref(0).unwrap());
                let len = blob.len() as i32;

                if let Some(tmp) = tmp_ostream.as_mut() {
                    let _ = tmp.write_all(&blob);
                }

                entries.push(EntryV3::new(tile_id, offset, len as u32, 1));
                hash_to_offset_len.insert(hsh, (offset, len as u64));
                offset += len as u64;
            }
        }
        if let Some(mut tmp) = tmp_ostream {
            let _ = tmp.flush();
        }
    }

    // finalize PMTiles archive.
    {
        entries.sort_by(pmtiles::entryv3_cmp);

        let (root_bytes, leaves_bytes, _num_leaves) =
            pmtiles::make_root_leaves(&compress_fn, pmtiles::COMPRESSION_GZIP, &entries);

        let mut header = pmtiles::HeaderV3 {
            min_zoom: m.minzoom as u8,
            max_zoom: m.maxzoom as u8,
            min_lon_e7: (m.minlon * 10000000.0) as i32,
            min_lat_e7: (m.minlat * 10000000.0) as i32,
            max_lon_e7: (m.maxlon * 10000000.0) as i32,
            max_lat_e7: (m.maxlat * 10000000.0) as i32,
            center_zoom: m.center_z as u8,
            center_lon_e7: (m.center_lon * 10000000.0) as i32,
            center_lat_e7: (m.center_lat * 10000000.0) as i32,
            ..Default::default()
        };

        let json_metadata = metadata_to_pmtiles_json(&m);

        drop(db);

        header.clustered = true;
        header.internal_compression = pmtiles::COMPRESSION_GZIP;

        if tile_compression {
            header.tile_compression = pmtiles::COMPRESSION_GZIP;
        } else {
            header.tile_compression = pmtiles::COMPRESSION_NONE;
        }

        if m.format == "pbf" {
            header.tile_type = pmtiles::TILETYPE_MVT;
        } else if m.format == "png" {
            header.tile_type = pmtiles::TILETYPE_PNG;
        } else {
            header.tile_type = pmtiles::TILETYPE_UNKNOWN;
        }

        header.root_dir_offset = 127;
        header.root_dir_bytes = root_bytes.len() as u64;

        header.json_metadata_offset = header.root_dir_offset + header.root_dir_bytes;
        header.json_metadata_bytes = json_metadata.len() as u64;
        header.leaf_dirs_offset = header.json_metadata_offset + header.json_metadata_bytes;
        header.leaf_dirs_bytes = leaves_bytes.len() as u64;
        header.tile_data_offset = header.leaf_dirs_offset + header.leaf_dirs_bytes;
        header.tile_data_bytes = offset;

        header.addressed_tiles_count = tile_ids.len() as u64;
        header.tile_entries_count = entries.len() as u64;
        header.tile_contents_count = hash_to_offset_len.len() as u64;

        let tmp_istream = File::open(&tmpname).ok();

        let mut ostream = File::create(fname).ok().map(BufWriter::new);

        if let Some(ostream) = ostream.as_mut() {
            let header_str = header.serialize();
            let _ = ostream.write_all(&header_str);
            let _ = ostream.write_all(&root_bytes);
            let _ = ostream.write_all(&json_metadata);
            let _ = ostream.write_all(&leaves_bytes);
            if let Some(mut tmp_istream) = tmp_istream.as_ref() {
                let _ = std::io::copy(&mut tmp_istream, ostream);
            }
        }

        drop(tmp_istream);
        let _ = fs::remove_file(&tmpname);
        if let Some(mut ostream) = ostream {
            let _ = ostream.flush();
        }
    }
}

// this should go away if we get rid of temporary metadata DBs.
// it transforms the PMTiles header + json into string keys/string values
// consistent with mbtiles and dirtiles, for the test suite
pub fn pmtilesmeta2tmp(fname: &str, pmtiles_map: &[u8]) -> Connection {
    let db = match sqlite3_open("") {
        Ok(db) => db,
        Err(msg) => {
            eprintln!("Temporary db: {}", msg);
            exit(EXIT_SQLITE);
        }
    };
    if let Err(err) = sqlite3_exec(&db, c"CREATE TABLE metadata (name text, value text);") {
        eprintln!("Create metadata table: {}", err);
        exit(EXIT_SQLITE);
    }

    let header_s = pmtiles::header_bytes(pmtiles_map);
    let header = pmtiles::deserialize_header(&header_s);

    let sql = sqlite3_mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('minzoom', %d);",
        header.min_zoom as c_int
    );
    if let Err(err) = sqlite3_exec(&db, &sql) {
        eprintln!("set minzoom: {}", err);
    }

    let sql = sqlite3_mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('maxzoom', %d);",
        header.max_zoom as c_int
    );
    if let Err(err) = sqlite3_exec(&db, &sql) {
        eprintln!("set maxzoom: {}", err);
    }

    let center_lon = header.center_lon_e7 as f64 / 10000000.0;
    let center_lat = header.center_lat_e7 as f64 / 10000000.0;
    let sql = sqlite3_mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('center', '%f,%f,%d');",
        center_lon as c_double,
        center_lat as c_double,
        header.center_zoom as c_int
    );
    if let Err(err) = sqlite3_exec(&db, &sql) {
        eprintln!("set center: {}", err);
    }

    let minlon = header.min_lon_e7 as f64 / 10000000.0;
    let minlat = header.min_lat_e7 as f64 / 10000000.0;
    let maxlon = header.max_lon_e7 as f64 / 10000000.0;
    let maxlat = header.max_lat_e7 as f64 / 10000000.0;
    let sql = sqlite3_mprintf!(
        c"INSERT INTO metadata (name, value) VALUES ('bounds', '%f,%f,%f,%f');",
        minlon as c_double,
        minlat as c_double,
        maxlon as c_double,
        maxlat as c_double
    );
    if let Err(err) = sqlite3_exec(&db, &sql) {
        eprintln!("set bounds: {}", err);
    }

    let json_start = header.json_metadata_offset as usize;
    let json_s = &pmtiles_map[json_start..json_start + header.json_metadata_bytes as usize];
    let mut decompressed_json: Vec<u8> = Vec::new();

    if header.internal_compression == pmtiles::COMPRESSION_NONE {
        decompressed_json = json_s.to_vec();
    } else if header.internal_compression == pmtiles::COMPRESSION_GZIP {
        decompress(json_s, &mut decompressed_json);
    } else {
        eprintln!(
            "Unknown or unsupported pmtiles compression: {}",
            header.internal_compression
        );
        exit(EXIT_OPEN);
    }

    // json_begin_string() takes the C string, which ends at the first NUL.
    let json_end = decompressed_json
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(decompressed_json.len());
    let mut jp = JsonPull::begin_string(&decompressed_json[..json_end]);
    let o = match jp.read_tree() {
        Some(o) => o,
        None => {
            eprintln!(
                "{}: metadata parsing error: {}",
                fname,
                jp.error.unwrap_or("(null)")
            );
            exit(EXIT_JSON);
        }
    };

    if o.root().type_() != JsonType::Hash {
        eprintln!("{}: bad metadata format", fname);
        exit(EXIT_JSON);
    }

    let mut has_json = false;
    let mut buf = String::new();
    {
        let mut state = JsonWriter::new_string(&mut buf);
        state.nospace = true;
        state.json_write_hash();

        for (key, value) in o.root().entries() {
            // Check before reading, since string() asserts on the type.
            if key.type_() != JsonType::String {
                eprintln!("{}: non-string key in metadata", fname);
                continue;
            }

            let key = key.raw_string_bytes();
            if key == b"vector_layers" && value.type_() == JsonType::Array {
                has_json = true;
                state.nospace = true;
                state.json_write_string("vector_layers");
                state.nospace = true;
                state.json_write_json(&value.stringify());
            } else if key == b"tilestats" && value.type_() == JsonType::Hash {
                has_json = true;
                state.nospace = true;
                state.json_write_string("tilestats");
                state.nospace = true;
                state.json_write_json(&value.stringify());
            } else if key == b"strategies" && value.type_() == JsonType::Array {
                let v = c_str_bytes(&value.stringify_raw());
                let sql = sqlite3_mprintf!(
                    c"INSERT INTO metadata (name, value) VALUES ('strategies', %Q);",
                    v.as_ptr()
                );
                if let Err(err) = sqlite3_exec(&db, &sql) {
                    eprintln!(
                        "set {} in metadata: {}",
                        c_str_bytes(key).to_string_lossy(),
                        err
                    );
                }
            } else if key == b"tippecanoe_decisions" && value.type_() == JsonType::Hash {
                let v = c_str_bytes(&value.stringify_raw());
                let sql = sqlite3_mprintf!(
                    c"INSERT INTO metadata (name, value) VALUES ('tippecanoe_decisions', %Q);",
                    v.as_ptr()
                );
                if let Err(err) = sqlite3_exec(&db, &sql) {
                    eprintln!(
                        "set {} in metadata: {}",
                        c_str_bytes(key).to_string_lossy(),
                        err
                    );
                }
            } else if value.type_() != JsonType::String {
                eprintln!("{}", c_str_bytes(key).to_string_lossy());
                eprintln!("{}: non-string in metadata", fname);
            } else {
                let k = c_str_bytes(key);
                let v = c_str_bytes(value.raw_string_bytes());
                let sql = sqlite3_mprintf!(
                    c"INSERT INTO metadata (name, value) VALUES (%Q, %Q);",
                    k.as_ptr(),
                    v.as_ptr()
                );
                if let Err(err) = sqlite3_exec(&db, &sql) {
                    eprintln!("set {} in metadata: {}", k.to_string_lossy(), err);
                }
            }
        }

        state.nospace = true;
        state.json_end_hash();
    }

    if has_json {
        let json = c_str(&buf);
        let sql = sqlite3_mprintf!(
            c"INSERT INTO metadata (name, value) VALUES ('json', %Q);",
            json.as_ptr()
        );
        if let Err(err) = sqlite3_exec(&db, &sql) {
            eprintln!("set json in metadata: {}", err);
        }
    }

    db
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dirtiles::{
        check_dir, dir_erase_zoom, dir_read_tile, dir_write_metadata, dir_write_tile, dirmeta2tmp,
        enumerate_dirtiles,
    };
    use crate::mbtiles::tests::{Rng, fnv, make_all};
    use crate::mbtiles::{mbtiles_close, mbtiles_open, mbtiles_write_metadata, merge_layermaps};

    fn rows(db: &Connection, out: &mut Vec<u8>) {
        let mut stmt = db
            .prepare("SELECT name, value FROM metadata ORDER BY rowid")
            .unwrap();
        let mut rows = stmt.raw_query();
        while let Ok(Some(row)) = rows.next() {
            out.extend_from_slice(b"row ");
            out.extend_from_slice(&column_bytes(row.get_ref(0).unwrap()));
            out.push(b'|');
            out.extend_from_slice(&column_bytes(row.get_ref(1).unwrap()));
            out.push(b'\n');
        }
    }

    fn rblob(rng: &mut Rng) -> Vec<u8> {
        let n = rng.next() % 300;
        (0..n).map(|_| (rng.next() & 0xFF) as u8).collect()
    }

    /// Writes tile directories and PMTiles archives for the metadata of the
    /// C++ harness, reads them back, and compares a dump of all of it with the
    /// same dump from the C++.
    #[test]
    fn storage_matches_cpp() {
        let tmp =
            std::env::temp_dir().join(format!("tippecanoe-rs-storage-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let base = tmp.join("out").to_string_lossy().into_owned();
        let mut out: Vec<u8> = Vec::new();

        let mut rng = Rng(42);
        let maps: Vec<_> = (0..3).map(|i| rng.rmap(i)).collect();
        let ms = make_all(&merge_layermaps(&maps));
        let av = vec!["harness".to_string()];

        for (mi, m) in ms.iter().enumerate() {
            writeln!(out, "== m{}", mi + 1).unwrap();

            // directory output
            let dir = base.clone() + "-dir" + &mi.to_string();
            check_dir(&dir, &av, true, false);
            let ntiles = if mi == 0 { 40 } else { 5 };
            for _ in 0..ntiles {
                let z = (rng.next() % 6) as i32;
                let x = (rng.next() % (1 << z)) as i32;
                let y = (rng.next() % (1 << z)) as i32;
                let fname = format!("{}/{}/{}/{}.pbf", dir, z, x, y);
                if fs::metadata(&fname).is_ok() {
                    continue;
                }
                let blob = rblob(&mut rng);
                dir_write_tile(&dir, z, x, y, &blob);
            }
            // a .mvt tile and some names to skip
            let _ = fs::create_dir(dir.clone() + "/2");
            let _ = fs::create_dir(dir.clone() + "/2/1");
            fs::write(dir.clone() + "/2/1/3.mvt", "mvt").unwrap();
            fs::write(dir.clone() + "/2/1/notes.txt", "x").unwrap();
            fs::write(dir.clone() + "/2/1/3.pbf.bak", "x").unwrap();
            let _ = fs::create_dir(dir.clone() + "/x7");
            dir_write_metadata(&dir, m);
            out.extend_from_slice(b"metadata.json=");
            out.extend_from_slice(&fs::read(dir.clone() + "/metadata.json").unwrap());

            let tiles = enumerate_dirtiles(&dir, 1, 4);
            for t in &tiles {
                writeln!(out, "tile {} {}", t.path(), fnv(&dir_read_tile(&dir, t))).unwrap();
            }
            dir_erase_zoom(&dir, 2);
            let tiles = enumerate_dirtiles(&dir, 0, 100);
            writeln!(out, "after erase {}", tiles.len()).unwrap();
            for t in &tiles {
                writeln!(out, "tile {}", t.path()).unwrap();
            }

            let db = dirmeta2tmp(&dir);
            rows(&db, &mut out);
            drop(db);

            fs::remove_file(dir.clone() + "/2/1/notes.txt").unwrap();
            fs::remove_file(dir.clone() + "/2/1/3.pbf.bak").unwrap();
            check_dir(&dir, &av, true, false);
            let tiles = enumerate_dirtiles(&dir, 0, 100);
            writeln!(out, "after check_dir {}", tiles.len()).unwrap();

            // pmtiles output
            let pm = base.clone() + &mi.to_string() + ".pmtiles";
            let _ = fs::remove_file(&pm);
            let outdb = mbtiles_open(&pm, &av, 0);
            let n = if mi == 0 {
                30000
            } else if mi == 2 {
                0
            } else {
                60
            };
            let mut id: u64 = 5 + rng.next() % 1000;
            let pool: Vec<Vec<u8>> = (0..20).map(|_| rblob(&mut rng)).collect();
            let mut prev = rblob(&mut rng);
            for _ in 0..n {
                let data = match rng.next() % 10 {
                    0..=2 => prev.clone(),
                    3..=5 => pool[(rng.next() % pool.len() as u64) as usize].clone(),
                    _ => rblob(&mut rng),
                };
                let t = pmtiles::tileid_to_zxy(id);
                crate::mbtiles::mbtiles_write_tile(
                    &outdb, t.z as i32, t.x as i32, t.y as i32, &data,
                );
                prev = data;
                id += 1 + if rng.next() % 10 == 0 {
                    rng.next() % 5000
                } else {
                    0
                };
            }
            mbtiles_write_metadata(&outdb, m, false);
            mbtiles_close(outdb, "harness");
            mbtiles_map_image_to_pmtiles(&pm, m.clone(), mi != 1, true, true);
            let archive = fs::read(&pm).unwrap();
            writeln!(out, "pmtiles {} {}", archive.len(), fnv(&archive)).unwrap();
            let h = pmtiles::deserialize_header(&archive[..127]);
            writeln!(
                out,
                "header root={} json={} leaves={} data={} addressed={} entries={} contents={}",
                h.root_dir_bytes,
                h.json_metadata_bytes,
                h.leaf_dirs_bytes,
                h.tile_data_bytes,
                h.addressed_tiles_count,
                h.tile_entries_count,
                h.tile_contents_count
            )
            .unwrap();

            let db = pmtilesmeta2tmp(&pm, &archive);
            rows(&db, &mut out);
            drop(db);

            let entries = pmtiles_entries_tms(&archive, 0, 32);
            let mut text = String::new();
            for e in &entries {
                text += &format!("{}/{}/{}@{}+{},", e.z, e.x, e.y, e.offset, e.length);
            }
            writeln!(out, "entries {} {}", entries.len(), fnv(text.as_bytes())).unwrap();
            let entries = pmtiles_entries_tms(&archive, 7, 12);
            writeln!(out, "entries 7-12 {}", entries.len()).unwrap();
            let mut i = 0;
            while i < entries.len() {
                let e = entries[i];
                let r = pmtiles_get_tile(&archive, e.z as i32, e.x as i32, e.y as i32);
                writeln!(out, "get {}/{}/{} {} {}", e.z, e.x, e.y, r.0, r.1).unwrap();
                i += 1 + entries.len() / 7;
            }
            let r = pmtiles_get_tile(&archive, 3, 1, 1);
            writeln!(out, "get 3/1/1 {} {}", r.0, r.1).unwrap();
            fs::remove_file(&pm).unwrap();
        }

        let _ = fs::remove_dir_all(&tmp);
        if (out.len(), fnv(&out)) != (637502, 6648167400525258637) {
            let path = std::env::temp_dir().join("tippecanoe-rs-storage.out");
            fs::write(&path, &out).unwrap();
            panic!("storage dump differs from the C++, see {}", path.display());
        }
    }

    #[test]
    fn suffix() {
        assert!(pmtiles_has_suffix("a.pmtiles"));
        assert!(pmtiles_has_suffix(".pmtiles"));
        assert!(!pmtiles_has_suffix("pmtiles"));
        assert!(!pmtiles_has_suffix("a.mbtiles"));
        assert!(!pmtiles_has_suffix("a.pmtiles.tmp"));
    }
}
