//! Globals that the C++ declares `extern` in main.hpp, options.hpp and mbtiles.hpp,
//! and defines in main.cpp, tile-join.cpp and mbtiles.cpp.
//!
//! Scalars are atomics read and written with `Relaxed` ordering, the way the C++
//! reads plain globals. Larger values sit behind a `RwLock` and are set while the
//! options are parsed, before any worker thread starts.

use std::collections::BTreeMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU64, AtomicUsize, Ordering};

use crate::geometry::ClipBbox;
use crate::json_logger::JsonLogger;
use crate::serial::SerialVal;

pub const MAX_ZOOM: i32 = 24;

// Not legal UTF-8 in the C++ ("\200size"), so it can't appear as a real
// attribute name. Rust strings must be valid UTF-8, so this uses U+0080 instead.
pub const ORDER_BY_SIZE: &str = "\u{80}size";

/// An `f64` that can be shared between threads, for the C++ `std::atomic<double>`
/// and for plain `double` globals.
pub struct AtomicF64(AtomicU64);

impl AtomicF64 {
    pub const fn new(v: f64) -> AtomicF64 {
        AtomicF64(AtomicU64::new(v.to_bits()))
    }

    pub fn load(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }

    pub fn store(&self, v: f64) {
        self.0.store(v.to_bits(), Ordering::Relaxed)
    }
}

pub static PREVENT: [AtomicI32; 256] = [const { AtomicI32::new(0) }; 256];
pub static ADDITIONAL: [AtomicI32; 256] = [const { AtomicI32::new(0) }; 256];

/// `prevent[c]` in the C++.
pub fn prevent(c: usize) -> i32 {
    PREVENT[c].load(Ordering::Relaxed)
}

/// `additional[c]` in the C++.
pub fn additional(c: usize) -> i32 {
    ADDITIONAL[c].load(Ordering::Relaxed)
}

pub fn set_prevent(c: usize, v: i32) {
    PREVENT[c].store(v, Ordering::Relaxed)
}

pub fn set_additional(c: usize, v: i32) {
    ADDITIONAL[c].store(v, Ordering::Relaxed)
}

pub static GEOMETRY_SCALE: AtomicI32 = AtomicI32::new(0);
pub static QUIET: AtomicI32 = AtomicI32::new(0);
pub static QUIET_PROGRESS: AtomicI32 = AtomicI32::new(0);
pub static LOGGER: JsonLogger = JsonLogger::new();
pub static PROGRESS_INTERVAL: AtomicF64 = AtomicF64::new(0.0);
pub static LAST_PROGRESS: AtomicF64 = AtomicF64::new(0.0);
pub static EXTRA_DETAIL: AtomicI32 = AtomicI32::new(-1);

pub static CPUS: AtomicUsize = AtomicUsize::new(0);
pub static TEMP_FILES: AtomicUsize = AtomicUsize::new(0);
pub static MEMSIZE: AtomicUsize = AtomicUsize::new(0);

pub static MAX_TILE_SIZE: AtomicUsize = AtomicUsize::new(500000);
pub static MAX_TILE_FEATURES: AtomicUsize = AtomicUsize::new(200000);
pub static CLUSTER_DISTANCE: AtomicI32 = AtomicI32::new(0);
pub static CLUSTER_MAXZOOM: AtomicI32 = AtomicI32::new(MAX_ZOOM);
pub static ATTRIBUTE_FOR_ID: RwLock<String> = RwLock::new(String::new());
pub static TINY_POLYGON_SIZE: AtomicI32 = AtomicI32::new(2);
pub static LIMIT_TILE_FEATURE_COUNT: AtomicUsize = AtomicUsize::new(0);
pub static LIMIT_TILE_FEATURE_COUNT_AT_MAXZOOM: AtomicUsize = AtomicUsize::new(0);
pub static SET_ATTRIBUTES: RwLock<BTreeMap<String, SerialVal>> = RwLock::new(BTreeMap::new());
pub static EXTEND_ZOOMS_MAX: AtomicI64 = AtomicI64::new(0);
pub static RETAIN_POINTS_MULTIPLIER: AtomicI32 = AtomicI32::new(1);
pub static MAXIMUM_STRING_ATTRIBUTE_LENGTH: AtomicUsize = AtomicUsize::new(0);
pub static PRESERVE_MULTIPLIER_DENSITY_THRESHOLD: AtomicU64 = AtomicU64::new(0);

pub static MAX_TILESTATS_ATTRIBUTES: AtomicUsize = AtomicUsize::new(1000);
pub static MAX_TILESTATS_SAMPLE_VALUES: AtomicUsize = AtomicUsize::new(1000);
pub static MAX_TILESTATS_VALUES: AtomicUsize = AtomicUsize::new(100);

#[derive(Clone, Debug)]
pub struct OrderField {
    pub name: String,
    pub descending: bool,
}

impl OrderField {
    pub fn new(name: String, descending: bool) -> OrderField {
        OrderField { name, descending }
    }
}

pub static ORDER_BY: RwLock<Vec<OrderField>> = RwLock::new(Vec::new());
pub static ORDER_BY_SIZE_ENABLED: AtomicBool = AtomicBool::new(false);

pub static CLIPBBOXES: RwLock<Vec<ClipBbox>> = RwLock::new(Vec::new());

/// Shorthand for loading a `Relaxed` atomic global, as in `get(&QUIET)`.
pub fn get<T: RelaxedLoad>(v: &T) -> T::Value {
    v.relaxed_load()
}

pub trait RelaxedLoad {
    type Value;
    fn relaxed_load(&self) -> Self::Value;
}

macro_rules! relaxed_load {
    ($atomic:ty, $value:ty) => {
        impl RelaxedLoad for $atomic {
            type Value = $value;
            fn relaxed_load(&self) -> $value {
                self.load(Ordering::Relaxed)
            }
        }
    };
}

relaxed_load!(AtomicBool, bool);
relaxed_load!(AtomicI32, i32);
relaxed_load!(AtomicI64, i64);
relaxed_load!(AtomicU64, u64);
relaxed_load!(AtomicUsize, usize);

impl RelaxedLoad for AtomicF64 {
    type Value = f64;
    fn relaxed_load(&self) -> f64 {
        self.load()
    }
}
