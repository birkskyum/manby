//! Port of projection.cpp.

pub fn lonlat2tile(lon: f64, lat: f64, zoom: i32) -> (i64, i64) {
    todo!("projection::lonlat2tile")
}

pub fn epsg3857totile(ix: f64, iy: f64, zoom: i32) -> (i64, i64) {
    todo!("projection::epsg3857totile")
}

pub fn tile2lonlat(x: i64, y: i64, zoom: i32) -> (f64, f64) {
    todo!("projection::tile2lonlat")
}

pub fn tiletoepsg3857(x: i64, y: i64, zoom: i32) -> (f64, f64) {
    todo!("projection::tiletoepsg3857")
}

pub fn set_projection_or_exit(optarg: &str) {
    todo!("projection::set_projection_or_exit")
}

pub struct Projection {
    pub name: &'static str,
    pub project: fn(f64, f64, i32) -> (i64, i64),
    pub unproject: fn(i64, i64, i32) -> (f64, f64),
    pub alias: &'static str,
}

pub static PROJECTIONS: [Projection; 2] = [
    Projection {
        name: "EPSG:4326",
        project: lonlat2tile,
        unproject: tile2lonlat,
        alias: "urn:ogc:def:crs:OGC:1.3:CRS84",
    },
    Projection {
        name: "EPSG:3857",
        project: epsg3857totile,
        unproject: tiletoepsg3857,
        alias: "urn:ogc:def:crs:EPSG::3857",
    },
];

/// The current projection, the C++ `projection` global.
pub fn projection() -> &'static Projection {
    todo!("projection::projection")
}

/// Selects quadkey or Hilbert ordering for `encode_index` and `decode_index`,
/// which the C++ does by assigning the function pointers.
pub fn set_index_hilbert(hilbert: bool) {
    todo!("projection::set_index_hilbert")
}

pub fn encode_index(wx: u32, wy: u32) -> u64 {
    todo!("projection::encode_index")
}

pub fn decode_index(index: u64) -> (u32, u32) {
    todo!("projection::decode_index")
}

pub fn encode_quadkey(wx: u32, wy: u32) -> u64 {
    todo!("projection::encode_quadkey")
}

pub fn decode_quadkey(index: u64) -> (u32, u32) {
    todo!("projection::decode_quadkey")
}

pub fn encode_hilbert(wx: u32, wy: u32) -> u64 {
    todo!("projection::encode_hilbert")
}

pub fn decode_hilbert(index: u64) -> (u32, u32) {
    todo!("projection::decode_hilbert")
}

pub fn encode_vertex(wx: u32, wy: u32) -> u64 {
    todo!("projection::encode_vertex")
}
