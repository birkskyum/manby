//! Port of pmtiles/pmtiles.hpp, the PMTiles v3 format library.
//!
//! The C++ throws exceptions that nothing in tippecanoe catches, so they end the
//! program through `std::terminate`. `throw_uncaught` does the same here.
//! Functions that take `std::function` callbacks take `&dyn Fn` instead.

pub const TILETYPE_UNKNOWN: u8 = 0x0;
pub const TILETYPE_MVT: u8 = 0x1;
pub const TILETYPE_PNG: u8 = 0x2;
pub const TILETYPE_JPEG: u8 = 0x3;
pub const TILETYPE_WEBP: u8 = 0x4;

pub const COMPRESSION_UNKNOWN: u8 = 0x0;
pub const COMPRESSION_NONE: u8 = 0x1;
pub const COMPRESSION_GZIP: u8 = 0x2;
pub const COMPRESSION_BROTLI: u8 = 0x3;
pub const COMPRESSION_ZSTD: u8 = 0x4;

/// The callback type of `std::function<std::string(const std::string &, uint8_t)>`.
pub type CompressFn<'a> = &'a dyn Fn(&[u8], u8) -> Vec<u8>;

/// An exception that escapes `main` ends the program the way the C++ runtime
/// does: it names the exception on stderr and aborts.
pub fn throw_uncaught(type_name: &str, what: &str) -> ! {
    if cfg!(target_os = "macos") {
        eprintln!("libc++abi: terminating due to uncaught exception of type {type_name}: {what}");
    } else {
        eprintln!("terminate called after throwing an instance of '{type_name}'");
        eprintln!("  what():  {what}");
    }
    std::process::abort();
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeaderV3 {
    pub root_dir_offset: u64,
    pub root_dir_bytes: u64,
    pub json_metadata_offset: u64,
    pub json_metadata_bytes: u64,
    pub leaf_dirs_offset: u64,
    pub leaf_dirs_bytes: u64,
    pub tile_data_offset: u64,
    pub tile_data_bytes: u64,
    pub addressed_tiles_count: u64,
    pub tile_entries_count: u64,
    pub tile_contents_count: u64,
    pub clustered: bool,
    pub internal_compression: u8,
    pub tile_compression: u8,
    pub tile_type: u8,
    pub min_zoom: u8,
    pub max_zoom: u8,
    pub min_lon_e7: i32,
    pub min_lat_e7: i32,
    pub max_lon_e7: i32,
    pub max_lat_e7: i32,
    pub center_zoom: u8,
    pub center_lon_e7: i32,
    pub center_lat_e7: i32,
}

impl HeaderV3 {
    // WARNING: this is limited to little-endian
    pub fn serialize(&self) -> Vec<u8> {
        let mut ss: Vec<u8> = Vec::with_capacity(127);
        ss.extend_from_slice(b"PMTiles");
        let version: u8 = 3;
        ss.push(version);
        ss.extend_from_slice(&self.root_dir_offset.to_le_bytes());
        ss.extend_from_slice(&self.root_dir_bytes.to_le_bytes());
        ss.extend_from_slice(&self.json_metadata_offset.to_le_bytes());
        ss.extend_from_slice(&self.json_metadata_bytes.to_le_bytes());
        ss.extend_from_slice(&self.leaf_dirs_offset.to_le_bytes());
        ss.extend_from_slice(&self.leaf_dirs_bytes.to_le_bytes());
        ss.extend_from_slice(&self.tile_data_offset.to_le_bytes());
        ss.extend_from_slice(&self.tile_data_bytes.to_le_bytes());
        ss.extend_from_slice(&self.addressed_tiles_count.to_le_bytes());
        ss.extend_from_slice(&self.tile_entries_count.to_le_bytes());
        ss.extend_from_slice(&self.tile_contents_count.to_le_bytes());

        let mut clustered_val: u8 = 0x0;
        if self.clustered {
            clustered_val = 0x1;
        }

        ss.push(clustered_val);
        ss.push(self.internal_compression);
        ss.push(self.tile_compression);
        ss.push(self.tile_type);
        ss.push(self.min_zoom);
        ss.push(self.max_zoom);
        ss.extend_from_slice(&self.min_lon_e7.to_le_bytes());
        ss.extend_from_slice(&self.min_lat_e7.to_le_bytes());
        ss.extend_from_slice(&self.max_lon_e7.to_le_bytes());
        ss.extend_from_slice(&self.max_lat_e7.to_le_bytes());
        ss.push(self.center_zoom);
        ss.extend_from_slice(&self.center_lon_e7.to_le_bytes());
        ss.extend_from_slice(&self.center_lat_e7.to_le_bytes());

        ss
    }
}

fn copy_u64(s: &[u8], pos: usize) -> u64 {
    u64::from_le_bytes(s[pos..pos + 8].try_into().unwrap())
}

fn copy_i32(s: &[u8], pos: usize) -> i32 {
    i32::from_le_bytes(s[pos..pos + 4].try_into().unwrap())
}

pub fn deserialize_header(s: &[u8]) -> HeaderV3 {
    if s.len() < 7 || &s[0..7] != b"PMTiles" {
        throw_uncaught(
            "pmtiles::pmtiles_magic_number_exception",
            "pmtiles magic number exception",
        );
    }
    if s.len() != 127 || s[7] != 0x3 {
        throw_uncaught(
            "pmtiles::pmtiles_version_exception",
            "pmtiles version: must be 3",
        );
    }
    HeaderV3 {
        root_dir_offset: copy_u64(s, 8),
        root_dir_bytes: copy_u64(s, 16),
        json_metadata_offset: copy_u64(s, 24),
        json_metadata_bytes: copy_u64(s, 32),
        leaf_dirs_offset: copy_u64(s, 40),
        leaf_dirs_bytes: copy_u64(s, 48),
        tile_data_offset: copy_u64(s, 56),
        tile_data_bytes: copy_u64(s, 64),
        addressed_tiles_count: copy_u64(s, 72),
        tile_entries_count: copy_u64(s, 80),
        tile_contents_count: copy_u64(s, 88),
        clustered: s[96] == 0x1,
        internal_compression: s[97],
        tile_compression: s[98],
        tile_type: s[99],
        min_zoom: s[100],
        max_zoom: s[101],
        min_lon_e7: copy_i32(s, 102),
        min_lat_e7: copy_i32(s, 106),
        max_lon_e7: copy_i32(s, 110),
        max_lat_e7: copy_i32(s, 114),
        center_zoom: s[118],
        center_lon_e7: copy_i32(s, 119),
        center_lat_e7: copy_i32(s, 123),
    }
}

/// `std::string header_s{pmtiles_map, 127}`. The C++ reads 127 bytes even from a
/// shorter archive, which finds the zero fill at the end of the mapped page, so
/// a short archive is padded with zeros here.
pub fn header_bytes(pmtiles_map: &[u8]) -> Vec<u8> {
    let mut s = vec![0u8; 127];
    let n = pmtiles_map.len().min(127);
    s[..n].copy_from_slice(&pmtiles_map[..n]);
    s
}

/// `pmtiles::zxy`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Zxy {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl Zxy {
    pub fn new(z: i32, x: i32, y: i32) -> Zxy {
        Zxy {
            z: z as u8,
            x: x as u32,
            y: y as u32,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EntryV3 {
    pub tile_id: u64,
    pub offset: u64,
    pub length: u32,
    pub run_length: u32,
}

impl EntryV3 {
    pub fn new(tile_id: u64, offset: u64, length: u32, run_length: u32) -> EntryV3 {
        EntryV3 {
            tile_id,
            offset,
            length,
            run_length,
        }
    }
}

/// `entryv3_cmp`.
pub fn entryv3_cmp(a: &EntryV3, b: &EntryV3) -> std::cmp::Ordering {
    a.tile_id.cmp(&b.tile_id)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EntryZxy {
    pub z: u8,
    pub x: u32,
    pub y: u32,
    pub offset: u64,
    pub length: u32,
}

impl EntryZxy {
    pub fn new(z: u8, x: u32, y: u32, offset: u64, length: u32) -> EntryZxy {
        EntryZxy {
            z,
            x,
            y,
            offset,
            length,
        }
    }
}

const MAX_VARINT_LENGTH: isize = (u64::BITS / 7 + 1) as isize;

fn throw_varint_too_long() -> ! {
    throw_uncaught(
        "pmtiles::varint_too_long_exception",
        "varint too long exception",
    )
}

fn throw_end_of_buffer() -> ! {
    throw_uncaught(
        "pmtiles::end_of_buffer_exception",
        "end of buffer exception",
    )
}

// from https://github.com/mapbox/protozero/blob/master/include/protozero/varint.hpp
/// `data[*pos..]` is the varint, and `data.len()` is the end of the buffer.
fn decode_varint_impl(data: &[u8], pos: &mut usize) -> u64 {
    let begin = *pos;
    let iend = data.len();
    let mut p = begin;
    let mut val: u64 = 0;

    if iend as isize - begin as isize >= MAX_VARINT_LENGTH {
        // fast path
        #[allow(clippy::never_loop)]
        loop {
            let mut b = data[p] as i8 as i64;
            p += 1;
            val = (b as u64) & 0x7f;
            if b >= 0 {
                break;
            }
            b = data[p] as i8 as i64;
            p += 1;
            val |= ((b as u64) & 0x7f) << 7;
            if b >= 0 {
                break;
            }
            b = data[p] as i8 as i64;
            p += 1;
            val |= ((b as u64) & 0x7f) << 14;
            if b >= 0 {
                break;
            }
            b = data[p] as i8 as i64;
            p += 1;
            val |= ((b as u64) & 0x7f) << 21;
            if b >= 0 {
                break;
            }
            b = data[p] as i8 as i64;
            p += 1;
            val |= ((b as u64) & 0x7f) << 28;
            if b >= 0 {
                break;
            }
            b = data[p] as i8 as i64;
            p += 1;
            val |= ((b as u64) & 0x7f) << 35;
            if b >= 0 {
                break;
            }
            b = data[p] as i8 as i64;
            p += 1;
            val |= ((b as u64) & 0x7f) << 42;
            if b >= 0 {
                break;
            }
            b = data[p] as i8 as i64;
            p += 1;
            val |= ((b as u64) & 0x7f) << 49;
            if b >= 0 {
                break;
            }
            b = data[p] as i8 as i64;
            p += 1;
            val |= ((b as u64) & 0x7f) << 56;
            if b >= 0 {
                break;
            }
            b = data[p] as i8 as i64;
            p += 1;
            val |= ((b as u64) & 0x01) << 63;
            if b >= 0 {
                break;
            }
            throw_varint_too_long();
        }
    } else {
        let mut shift: u32 = 0;
        while p != iend && (data[p] as i8) < 0 {
            val |= ((data[p] as i8 as i64 as u64) & 0x7f) << shift;
            p += 1;
            shift += 7;
        }
        if p == iend {
            throw_end_of_buffer();
        }
        val |= (data[p] as i8 as i64 as u64) << shift;
        p += 1;
    }

    *pos = p;
    val
}

fn decode_varint(data: &[u8], pos: &mut usize) -> u64 {
    // If this is a one-byte varint, decode it here.
    if data.len() != *pos && (data[*pos] as u64 & 0x80) == 0 {
        let val = data[*pos] as u64;
        *pos += 1;
        return val;
    }
    // If this varint is more than one byte, defer to complete implementation.
    decode_varint_impl(data, pos)
}

fn rotate(n: i64, x: &mut i64, y: &mut i64, rx: i64, ry: i64) {
    if ry == 0 {
        if rx == 1 {
            *x = n - 1 - *x;
            *y = n - 1 - *y;
        }
        std::mem::swap(x, y);
    }
}

fn t_on_level(z: u8, pos: u64) -> Zxy {
    let n: i64 = 1i64 << z;
    let mut rx: i64;
    let mut ry: i64;
    let mut s: i64;
    let mut t: i64 = pos as i64;
    let mut tx: i64 = 0;
    let mut ty: i64 = 0;

    s = 1;
    while s < n {
        rx = 1 & (t / 2);
        ry = 1 & (t ^ rx);
        rotate(s, &mut tx, &mut ty, rx, ry);
        tx += s * rx;
        ty += s * ry;
        t /= 4;
        s *= 2;
    }
    Zxy::new(z as i32, tx as i32, ty as i32)
}

fn write_varint(data: &mut Vec<u8>, mut value: u64) -> i32 {
    let mut n = 1;

    while value >= 0x80 {
        data.push(((value & 0x7f) | 0x80) as u8);
        value >>= 7;
        n += 1;
    }
    data.push(value as u8);

    n
}

// TMS order
fn colmajor_cmp(a: &EntryZxy, b: &EntryZxy) -> std::cmp::Ordering {
    if a.z != b.z {
        return a.z.cmp(&b.z);
    }
    if a.x != b.x {
        return a.x.cmp(&b.x);
    }
    b.y.cmp(&a.y)
}

// use a 0 length entry as a null value.
fn find_tile(entries: &[EntryV3], tile_id: u64) -> EntryV3 {
    let mut m: i32 = 0;
    let mut n: i32 = (entries.len() as i32).wrapping_sub(1);
    while m <= n {
        let k = (n + m) >> 1;
        // PORT: the C++ narrows the unsigned difference to int, so tile ids that
        // differ by 2^31 or more can compare the wrong way. Kept as it is.
        let cmp = tile_id.wrapping_sub(entries[k as usize].tile_id) as i32;
        if cmp > 0 {
            m = k + 1;
        } else if cmp < 0 {
            n = k - 1;
        } else {
            return entries[k as usize];
        }
    }

    if n >= 0 {
        if entries[n as usize].run_length == 0 {
            return entries[n as usize];
        }
        if tile_id.wrapping_sub(entries[n as usize].tile_id) < entries[n as usize].run_length as u64
        {
            return entries[n as usize];
        }
    }

    EntryV3::new(0, 0, 0, 0)
}

fn throw_overflow(what: &str) -> ! {
    throw_uncaught("std::overflow_error", what)
}

pub fn tileid_to_zxy(tileid: u64) -> Zxy {
    let mut acc: u64 = 0;
    for t_z in 0u8..32 {
        let num_tiles: u64 = ((1i64 << t_z) * (1i64 << t_z)) as u64;
        if acc + num_tiles > tileid {
            return t_on_level(t_z, tileid - acc);
        }
        acc += num_tiles;
    }
    throw_overflow("tile zoom exceeds 64-bit limit");
}

pub fn zxy_to_tileid(z: u8, x: u32, y: u32) -> u64 {
    if z > 31 {
        throw_overflow("tile zoom exceeds 64-bit limit");
    }
    if x > (1u32 << z).wrapping_sub(1) || y > (1u32 << z).wrapping_sub(1) {
        throw_overflow("tile x/y outside zoom level bounds");
    }
    let mut acc: u64 = 0;
    for t_z in 0..z {
        acc += ((1i64 << t_z) * (1i64 << t_z)) as u64;
    }
    let n: i64 = 1i64 << z;
    let mut rx: i64;
    let mut ry: i64;
    let mut s: i64;
    let mut d: i64 = 0;
    let mut tx: i64 = x as i64;
    let mut ty: i64 = y as i64;
    s = n / 2;
    while s > 0 {
        rx = ((tx & s) > 0) as i64;
        ry = ((ty & s) > 0) as i64;
        d += s * s * ((3 * rx) ^ ry);
        rotate(s, &mut tx, &mut ty, rx, ry);
        s /= 2;
    }
    acc + d as u64
}

// returns an uncompressed byte buffer
pub fn serialize_directory(entries: &[EntryV3]) -> Vec<u8> {
    let mut data: Vec<u8> = Vec::new();

    write_varint(&mut data, entries.len() as u64);

    let mut last_id: u64 = 0;
    for entry in entries {
        write_varint(&mut data, entry.tile_id.wrapping_sub(last_id));
        last_id = entry.tile_id;
    }

    for entry in entries {
        write_varint(&mut data, entry.run_length as u64);
    }

    for entry in entries {
        write_varint(&mut data, entry.length as u64);
    }

    for i in 0..entries.len() {
        if i > 0
            && entries[i].offset
                == entries[i - 1]
                    .offset
                    .wrapping_add(entries[i - 1].length as u64)
        {
            write_varint(&mut data, 0);
        } else {
            write_varint(&mut data, entries[i].offset.wrapping_add(1));
        }
    }

    data
}

// takes an uncompressed byte buffer
pub fn deserialize_directory(decompressed: &[u8]) -> Vec<EntryV3> {
    let mut t: usize = 0;

    let num_entries = decode_varint(decompressed, &mut t);

    let mut result: Vec<EntryV3> = Vec::new();
    result.resize(num_entries as usize, EntryV3::default());

    let mut last_id: u64 = 0;
    for entry in result.iter_mut() {
        let tile_id = last_id.wrapping_add(decode_varint(decompressed, &mut t));
        entry.tile_id = tile_id;
        last_id = tile_id;
    }

    for entry in result.iter_mut() {
        entry.run_length = decode_varint(decompressed, &mut t) as u32;
    }

    for entry in result.iter_mut() {
        entry.length = decode_varint(decompressed, &mut t) as u32;
    }

    for i in 0..result.len() {
        let tmp = decode_varint(decompressed, &mut t);

        if i > 0 && tmp == 0 {
            result[i].offset = result[i - 1]
                .offset
                .wrapping_add(result[i - 1].length as u64);
        } else {
            result[i].offset = tmp.wrapping_sub(1);
        }
    }

    // assert the directory has been fully consumed
    if t != decompressed.len() {
        eprintln!("Error: malformed pmtiles directory");
        std::process::exit(crate::errors::EXIT_FAILURE);
    }

    result
}

pub fn build_root_leaves(
    mycompress: CompressFn<'_>,
    compression: u8,
    entries: &[EntryV3],
    leaf_size: i32,
) -> (Vec<u8>, Vec<u8>, i32) {
    let mut root_entries: Vec<EntryV3> = Vec::new();
    let mut leaves_bytes: Vec<u8> = Vec::new();
    let mut num_leaves = 0;
    let mut i: usize = 0;
    while i < entries.len() {
        num_leaves += 1;
        let mut end = (i as i32).wrapping_add(leaf_size);
        if i + leaf_size as usize > entries.len() {
            end = entries.len() as i32;
        }
        let subentries = &entries[i..end as usize];
        let uncompressed_leaf = serialize_directory(subentries);
        let compressed_leaf = mycompress(&uncompressed_leaf, compression);
        root_entries.push(EntryV3::new(
            entries[i].tile_id,
            leaves_bytes.len() as u64,
            compressed_leaf.len() as u32,
            0,
        ));
        leaves_bytes.extend_from_slice(&compressed_leaf);
        i += leaf_size as usize;
    }
    let uncompressed_root = serialize_directory(&root_entries);
    let compressed_root = mycompress(&uncompressed_root, compression);
    (compressed_root, leaves_bytes, num_leaves)
}

pub fn make_root_leaves(
    mycompress: CompressFn<'_>,
    compression: u8,
    entries: &[EntryV3],
) -> (Vec<u8>, Vec<u8>, i32) {
    let test_bytes = serialize_directory(entries);
    let compressed = mycompress(&test_bytes, compression);
    if compressed.len() <= 16384 - 127 {
        return (compressed, Vec::new(), 0);
    }
    let mut leaf_size = 4096;
    loop {
        let (root_bytes, leaves_bytes, num_leaves) =
            build_root_leaves(mycompress, compression, entries, leaf_size);
        if root_bytes.len() < 16384 - 127 {
            return (root_bytes, leaves_bytes, num_leaves);
        }
        leaf_size *= 2;
    }
}

pub fn collect_entries(
    decompress: CompressFn<'_>,
    tile_entries: &mut Vec<EntryZxy>,
    pmtiles_map: &[u8],
    h: &HeaderV3,
    dir_offset: u64,
    dir_len: u64,
) {
    let dir_s = &pmtiles_map[dir_offset as usize..(dir_offset + dir_len) as usize];
    let decompressed_dir = decompress(dir_s, h.internal_compression);

    let dir_entries = deserialize_directory(&decompressed_dir);
    for entry in &dir_entries {
        if entry.run_length == 0 {
            collect_entries(
                decompress,
                tile_entries,
                pmtiles_map,
                h,
                h.leaf_dirs_offset + entry.offset,
                entry.length as u64,
            );
        } else {
            let mut i = entry.tile_id;
            while i < entry.tile_id + entry.run_length as u64 {
                let zxy = tileid_to_zxy(i);
                tile_entries.push(EntryZxy::new(
                    zxy.z,
                    zxy.x,
                    zxy.y,
                    h.tile_data_offset + entry.offset,
                    entry.length,
                ));
                i += 1;
            }
        }
    }
}

pub fn entries_tms(decompress: CompressFn<'_>, pmtiles_map: &[u8]) -> Vec<EntryZxy> {
    let header_s = header_bytes(pmtiles_map);
    let header = deserialize_header(&header_s);

    let mut tile_entries: Vec<EntryZxy> = Vec::new();

    collect_entries(
        decompress,
        &mut tile_entries,
        pmtiles_map,
        &header,
        header.root_dir_offset,
        header.root_dir_bytes,
    );
    tile_entries.sort_by(colmajor_cmp);
    tile_entries
}

pub fn get_tile(
    decompress: CompressFn<'_>,
    pmtiles_map: &[u8],
    z: u8,
    x: u32,
    y: u32,
) -> (u64, u32) {
    let tile_id = zxy_to_tileid(z, x, y);

    let header_s = header_bytes(pmtiles_map);
    let h = deserialize_header(&header_s);

    let mut dir_offset: u64 = h.root_dir_offset;
    let mut dir_length: u32 = h.root_dir_bytes as u32;
    for _depth in 0..=3 {
        let dir_s = &pmtiles_map[dir_offset as usize..dir_offset as usize + dir_length as usize];
        let decompressed_dir = decompress(dir_s, h.internal_compression);
        let dir_entries = deserialize_directory(&decompressed_dir);
        let entry = find_tile(&dir_entries, tile_id);

        if entry.length > 0 {
            if entry.run_length > 0 {
                return (h.tile_data_offset + entry.offset, entry.length);
            } else {
                dir_offset = h.leaf_dirs_offset + entry.offset;
                dir_length = entry.length;
            }
        } else {
            return (0, 0);
        }
    }

    (0, 0)
}
