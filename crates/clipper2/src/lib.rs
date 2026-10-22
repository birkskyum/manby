//! Rust port of the parts of Clipper2 1.4.0 (Angus Johnson's polygon clipping library,
//! as vendored in tippecanoe) that tippecanoe uses: the `Clipper64` engine with closed
//! and open subjects, closed clips, and solutions returned as paths.
//!
//! The port follows the C++ line by line, including the floating point expressions and
//! libc++'s `std::sort`, so that it returns the same paths in the same order.

mod clipper_core;
mod clipper_engine;
#[doc(hidden)]
pub mod libcxx_sort;

pub use clipper_core::{FillRule, Path64, Paths64, Point64};
pub use clipper_engine::{ClipType, Clipper64};
