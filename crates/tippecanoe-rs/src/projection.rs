//! Port of projection.cpp.

use std::f64::consts::PI as M_PI;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::errors::EXIT_ARGS;

// http://wiki.openstreetmap.org/wiki/Slippy_map_tilenames
#[allow(clippy::manual_clamp)]
pub fn lonlat2tile(mut lon: f64, mut lat: f64, zoom: i32) -> (i64, i64) {
    // Place infinite and NaN coordinates off the edge of the Mercator plane

    let mut bad_lon = false;

    if lat.is_infinite() || lat.is_nan() {
        lat = 89.9;
    }
    if lon.is_infinite() || lon.is_nan() {
        // Keep these far enough from the plane that they don't get
        // moved back into it by 360-degree offsetting

        lon = 720.0;
        bad_lon = true;
    }

    // Must limit latitude somewhere to prevent overflow.
    // 89.9 degrees latitude is 0.621 worlds beyond the edge of the flat earth,
    // hopefully far enough out that there are few expectations about the shape.
    if lat < -89.9 {
        lat = -89.9;
    }
    if lat > 89.9 {
        lat = 89.9;
    }

    if lon < -360.0 && !bad_lon {
        lon = -360.0;
    }
    if lon > 360.0 && !bad_lon {
        lon = 360.0;
    }

    let lat_rad = lat * M_PI / 180.0;
    let n: u64 = 1u64 << zoom;

    let llx = (n as f64 * ((lon + 180.0) / 360.0)).round() as i64;
    let lly = (n as f64 * (1.0 - ((lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / M_PI)) / 2.0).round()
        as i64;

    (llx, lly)
}

// http://wiki.openstreetmap.org/wiki/Slippy_map_tilenames
pub fn tile2lonlat(x: i64, y: i64, zoom: i32) -> (f64, f64) {
    let n: u64 = 1u64 << zoom;
    let lon = 360.0 * x as f64 / n as f64 - 180.0;
    let lat = (M_PI * (1.0 - 2.0 * y as f64 / n as f64)).sinh().atan() * 180.0 / M_PI;
    (lon, lat)
}

pub fn epsg3857totile(mut ix: f64, mut iy: f64, zoom: i32) -> (i64, i64) {
    // Place infinite and NaN coordinates off the edge of the Mercator plane

    if iy.is_infinite() || iy.is_nan() {
        iy = 40000000.0;
    }
    if ix.is_infinite() || ix.is_nan() {
        ix = 40000000.0;
    }

    let mut x = (ix * (1i64 << 31) as f64 / 6378137.0 / M_PI + (1i64 << 31) as f64).round() as i64;
    let mut y = (((1i64 << 32) - 1) as f64
        - (iy * (1i64 << 31) as f64 / 6378137.0 / M_PI + (1i64 << 31) as f64))
        .round() as i64;

    if zoom != 0 {
        x = (x as f64 / (1i64 << (32 - zoom)) as f64).round() as i64;
        y = (y as f64 / (1i64 << (32 - zoom)) as f64).round() as i64;
    }

    (x, y)
}

pub fn tiletoepsg3857(mut ix: i64, mut iy: i64, zoom: i32) -> (f64, f64) {
    if zoom != 0 {
        ix <<= 32 - zoom;
        iy <<= 32 - zoom;
    }

    let ox = (ix.wrapping_sub(1i64 << 31)) as f64 * M_PI * 6378137.0 / (1i64 << 31) as f64;
    let oy = ((1i64 << 32) - 1).wrapping_sub(iy).wrapping_sub(1i64 << 31) as f64 * M_PI * 6378137.0
        / (1i64 << 31) as f64;
    (ox, oy)
}

// https://en.wikipedia.org/wiki/Hilbert_curve

fn hilbert_rot(n: u64, x: &mut u32, y: &mut u32, rx: u64, ry: u64) {
    if ry == 0 {
        if rx == 1 {
            *x = n.wrapping_sub(1).wrapping_sub(*x as u64) as u32;
            *y = n.wrapping_sub(1).wrapping_sub(*y as u64) as u32;
        }

        std::mem::swap(x, y);
    }
}

fn hilbert_xy2d(n: u64, mut x: u32, mut y: u32) -> u64 {
    let mut d: u64 = 0;

    let mut s = n / 2;
    while s > 0 {
        let rx: u64 = ((x as u64 & s) != 0) as u64;
        let ry: u64 = ((y as u64 & s) != 0) as u64;

        d = d.wrapping_add(s.wrapping_mul(s).wrapping_mul((3 * rx) ^ ry));
        hilbert_rot(s, &mut x, &mut y, rx, ry);
        s /= 2;
    }

    d
}

fn hilbert_d2xy(n: u64, d: u64, x: &mut u32, y: &mut u32) {
    let mut t = d;

    *x = 0;
    *y = 0;
    let mut s: u64 = 1;
    while s < n {
        let rx = 1 & (t / 2);
        let ry = 1 & (t ^ rx);
        hilbert_rot(s, x, y, rx, ry);
        *x = (*x as u64).wrapping_add(s.wrapping_mul(rx)) as u32;
        *y = (*y as u64).wrapping_add(s.wrapping_mul(ry)) as u32;
        t /= 4;
        s *= 2;
    }
}

pub fn encode_hilbert(wx: u32, wy: u32) -> u64 {
    hilbert_xy2d(1u64 << 32, wx, wy)
}

pub fn decode_hilbert(index: u64) -> (u32, u32) {
    let mut wx = 0;
    let mut wy = 0;
    hilbert_d2xy(1u64 << 32, index, &mut wx, &mut wy);
    (wx, wy)
}

pub fn encode_quadkey(wx: u32, wy: u32) -> u64 {
    let mut out: u64 = 0;

    for i in 0..32 {
        let mut v: u64 = (((wx >> (32 - (i + 1))) & 1) << 1) as u64;
        v |= ((wy >> (32 - (i + 1))) & 1) as u64;
        v <<= 64 - 2 * (i + 1);

        out |= v;
    }

    out
}

const fn decode_tables() -> ([u8; 256], [u8; 256]) {
    let mut decodex = [0u8; 256];
    let mut decodey = [0u8; 256];

    let mut ix: u64 = 0;
    while ix < 256 {
        let mut xx: u64 = 0;
        let mut yy: u64 = 0;

        let mut i = 0;
        while i < 32 {
            xx |= ((ix >> (64 - 2 * (i + 1) + 1)) & 1) << (32 - (i + 1));
            yy |= ((ix >> (64 - 2 * (i + 1))) & 1) << (32 - (i + 1));
            i += 1;
        }

        decodex[ix as usize] = xx as u8;
        decodey[ix as usize] = yy as u8;
        ix += 1;
    }

    (decodex, decodey)
}

static DECODE_TABLES: ([u8; 256], [u8; 256]) = decode_tables();

pub fn decode_quadkey(index: u64) -> (u32, u32) {
    let (decodex, decodey) = &DECODE_TABLES;

    let mut wx: u32 = 0;
    let mut wy: u32 = 0;

    for i in 0..8 {
        wx |= (decodex[((index >> (8 * i)) & 0xFF) as usize] as u32) << (4 * i);
        wy |= (decodey[((index >> (8 * i)) & 0xFF) as usize] as u32) << (4 * i);
    }

    (wx, wy)
}

pub fn set_projection_or_exit(optarg: &str) {
    for (i, p) in PROJECTIONS.iter().enumerate() {
        if p.name == optarg {
            PROJECTION.store(i, Ordering::Relaxed);
            return;
        }
        if p.alias == optarg {
            PROJECTION.store(i, Ordering::Relaxed);
            return;
        }
    }

    eprintln!("Unknown projection (-s): {}", optarg);
    std::process::exit(EXIT_ARGS);
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

/// Index into `PROJECTIONS` of the current projection.
static PROJECTION: AtomicUsize = AtomicUsize::new(0);

/// The current projection, the C++ `projection` global.
pub fn projection() -> &'static Projection {
    &PROJECTIONS[PROJECTION.load(Ordering::Relaxed)]
}

/// Whether `encode_index` and `decode_index` use the Hilbert curve rather than quadkeys.
static INDEX_HILBERT: AtomicBool = AtomicBool::new(false);

/// Selects quadkey or Hilbert ordering for `encode_index` and `decode_index`,
/// which the C++ does by assigning the function pointers.
pub fn set_index_hilbert(hilbert: bool) {
    INDEX_HILBERT.store(hilbert, Ordering::Relaxed);
}

pub fn encode_index(wx: u32, wy: u32) -> u64 {
    if INDEX_HILBERT.load(Ordering::Relaxed) {
        encode_hilbert(wx, wy)
    } else {
        encode_quadkey(wx, wy)
    }
}

pub fn decode_index(index: u64) -> (u32, u32) {
    if INDEX_HILBERT.load(Ordering::Relaxed) {
        decode_hilbert(index)
    } else {
        decode_quadkey(index)
    }
}

pub fn encode_vertex(wx: u32, wy: u32) -> u64 {
    ((wx as u64) << 32) | wy as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quadkey_round_trip() {
        for &(x, y) in &[
            (0u32, 0u32),
            (1, 2),
            (0xFFFFFFFF, 0),
            (0x12345678, 0x9ABCDEF0),
        ] {
            assert_eq!(decode_quadkey(encode_quadkey(x, y)), (x, y));
            assert_eq!(decode_hilbert(encode_hilbert(x, y)), (x, y));
        }
        assert_eq!(encode_quadkey(0x80000000, 0), 0x8000000000000000);
        assert_eq!(encode_quadkey(0, 0x80000000), 0x4000000000000000);
        assert_eq!(encode_vertex(1, 2), 0x100000002);
    }

    #[test]
    fn projects() {
        assert_eq!(lonlat2tile(0.0, 0.0, 32), (1 << 31, 1 << 31));
        assert_eq!(lonlat2tile(-180.0, 0.0, 0), (0, 1));
        assert_eq!(tile2lonlat(0, 0, 0), (-180.0, 85.05112877980659));
        assert_eq!(epsg3857totile(0.0, 0.0, 32), (1 << 31, (1 << 31) - 1));
        assert_eq!(projection().name, "EPSG:4326");
    }
}
