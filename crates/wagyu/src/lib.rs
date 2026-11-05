//! Rust port of mapbox/wagyu 0.5.0, the polygon clipping and cleaning library vendored in
//! tippecanoe (`mapbox/geometry/wagyu`), including tippecanoe's change to `correct_tree`.
//!
//! Only `wagyu<long long>` is ported, since that is the only instantiation tippecanoe uses.
//! Each C++ header has a module of the same name. Raw pointers become indices into arenas,
//! and every traversal, sort and floating point expression follows the C++ order, so the
//! output (rings, ring order, start points and orientation) matches the C++ exactly.
//!
//! Not ported, because tippecanoe cannot reach them:
//! - `quick_clip.hpp` and other coordinate types.
//! - `interrupt.hpp`: tippecanoe does not define `USE_WAGYU_INTERRUPT`, so `interrupt_check`
//!   does nothing.
//! - the `DEBUG` printing helpers and a few unused ring helpers (see `ring.rs` and `util.rs`).
//!
//! ```
//! use manby_wagyu::{ClipType, FillType, Point, PolygonType, Wagyu};
//!
//! let square = vec![Point::new(0, 0), Point::new(10, 0), Point::new(10, 10), Point::new(0, 10), Point::new(0, 0)];
//! let mut w = Wagyu::new();
//! w.add_ring(&square, PolygonType::Subject);
//! let result = w.execute(ClipType::Union, FillType::Positive, FillType::Positive).unwrap();
//! assert_eq!(result, vec![vec![vec![
//!     Point::new(10, 0), Point::new(10, 10), Point::new(0, 10), Point::new(0, 0), Point::new(10, 0),
//! ]]]);
//! ```

// The port keeps the C++ shape: long parameter lists, hand-written swaps, index loops,
// negated float comparisons (NaN-sensitive) and branches that repeat a body.
#![allow(
    clippy::too_many_arguments,
    clippy::nonminimal_bool,
    clippy::needless_bool,
    clippy::needless_bool_assign,
    clippy::needless_range_loop,
    clippy::neg_cmp_op_on_partial_ord,
    clippy::manual_swap,
    clippy::manual_abs_diff,
    clippy::if_same_then_else,
    clippy::enum_variant_names
)]

mod active_bound_list;
mod almost_equal;
mod bound;
mod bubble_sort;
mod build_edges;
mod build_local_minima_list;
mod build_result;
mod config;
mod edge;
mod geometry;
mod intersect;
mod intersect_util;
mod local_minimum;
mod local_minimum_util;
mod point;
mod process_horizontal;
mod process_maxima;
mod ring;
mod ring_util;
mod scanbeam;
mod snap_rounding;
mod stable_sort;
mod topology_correction;
mod util;
mod vatti;
#[allow(clippy::module_inception)]
mod wagyu;

pub use config::{ClipType, Error, FillType, PolygonType};
pub use geometry::{BoundingBox, LinearRing, MultiPolygon, Point, Polygon};
pub use wagyu::Wagyu;
