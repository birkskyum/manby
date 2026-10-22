//! The parts of `mapbox/geometry` that wagyu uses, for `long long` coordinates.

/// `mapbox::geometry::point<long long>`
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Point {
    pub x: i64,
    pub y: i64,
}

impl Point {
    pub const fn new(x: i64, y: i64) -> Self {
        Point { x, y }
    }
}

/// `mapbox::geometry::linear_ring<long long>`
pub type LinearRing = Vec<Point>;

/// `mapbox::geometry::polygon<long long>`
pub type Polygon = Vec<LinearRing>;

/// `mapbox::geometry::multi_polygon<long long>`
pub type MultiPolygon = Vec<Polygon>;

/// `mapbox::geometry::box<long long>`
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BoundingBox {
    pub min: Point,
    pub max: Point,
}

impl BoundingBox {
    pub const fn new(min: Point, max: Point) -> Self {
        BoundingBox { min, max }
    }
}

/// `mapbox::geometry::point<double>`
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct PointF64 {
    pub x: f64,
    pub y: f64,
}
