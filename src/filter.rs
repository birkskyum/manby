//! Feature filters (`-j`, `-J`) with operators that tippecanoe's lack:
//! `like` and `ilike` (and `!like`, `!ilike`), and `intersects`.
//!
//! manby evaluates those itself, along with the `all`, `any`, `none` and
//! `attribute-filter` expressions around them, and hands every other
//! expression to tippecanoe-rs's evaluator, so that it keeps tippecanoe's
//! meaning, warnings and exit statuses. A filter without any of the three
//! goes to tippecanoe-rs's evaluator whole.
//!
//! tippecanoe-rs's evaluator only takes a whole filter, a hash of layer names
//! to expressions, so each expression it is given is held as a filter of its
//! own, `{"*": expression}`.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use tippecanoe::errors::EXIT_FILTER;
use tippecanoe::evaluator;
use tippecanoe::geometry::{Draw, VT_LINE, VT_LINETO, VT_MOVETO, VT_POINT, VT_POLYGON};
use tippecanoe::jsonpull::{JsonId, JsonPull, JsonRef, JsonTree, JsonType};
use tippecanoe::mvt::{
    MVT_BOOL, MVT_DOUBLE, MVT_FLOAT, MVT_INT, MVT_LINESTRING, MVT_NO_SUCH_KEY, MVT_POINT,
    MVT_POLYGON, MVT_SINT, MVT_STRING, MVT_UINT, MvtFeature, MvtLayer, MvtValue,
};
use tippecanoe::projection::lonlat2tile;

use crate::{Error, Result};

/// A parsed feature filter.
#[derive(Clone, Debug)]
pub struct Filter(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    tree: JsonTree,
    /// The expression of each layer in the filter, by its node in `tree`, if
    /// any of them has an operator of manby's. None if none has: the filter
    /// is then tippecanoe-rs's.
    exprs: Option<HashMap<JsonId, Expr>>,
}

#[derive(Debug)]
enum Expr {
    /// For tippecanoe-rs's evaluator: `{"*": expression}`.
    Port(JsonTree),
    All(Vec<Expr>),
    Any(Vec<Expr>),
    None(Vec<Expr>),
    AttributeFilter(String, Box<Expr>),
    Like {
        key: String,
        /// lowercased for `ilike`
        pattern: String,
        ignore_case: bool,
        negate: bool,
    },
    Intersects(Area),
}

/// A feature's geometry for "intersects": its draw commands in world coordinates
/// (2^32 to an axis, as `lonlat2tile` gives them at zoom 32), less `origin`.
pub struct FilterGeometry<'a> {
    pub draws: &'a [Draw],
    pub origin: (i64, i64),
    /// VT_POINT, VT_LINE or VT_POLYGON
    pub type_: i32,
}

/// The feature a filter is evaluated on, with the attribute getters of
/// tippecanoe-rs's `evaluate` and `evaluate_feature`.
enum Subject<'a> {
    Map(&'a HashMap<String, MvtValue>),
    Tile {
        feat: &'a MvtFeature,
        layer: &'a MvtLayer,
        z: i32,
    },
}

impl Subject<'_> {
    fn get(&self, key: &str) -> MvtValue {
        match *self {
            Subject::Map(attrs) => attrs.get(key).cloned().unwrap_or_else(no_such_key),
            Subject::Tile { feat, layer, z } => {
                if key == "$id" && feat.has_id {
                    let mut v = MvtValue {
                        type_: MVT_UINT,
                        ..Default::default()
                    };
                    v.numeric_value.set_uint_value(feat.id);
                    return v;
                }
                if key == "$type" {
                    let mut v = MvtValue {
                        type_: MVT_STRING,
                        ..Default::default()
                    };
                    if feat.type_ == MVT_POINT {
                        v.set_string_value("Point");
                    } else if feat.type_ == MVT_LINESTRING {
                        v.set_string_value("LineString");
                    } else if feat.type_ == MVT_POLYGON {
                        v.set_string_value("Polygon");
                    }
                    return v;
                }
                if key == "$zoom" {
                    let mut v = MvtValue {
                        type_: MVT_UINT,
                        ..Default::default()
                    };
                    v.numeric_value.set_uint_value(z as u64);
                    return v;
                }
                let mut i = 0;
                while i + 1 < feat.tags.len() {
                    if layer.keys[feat.tags[i] as usize] == key {
                        return layer.values[feat.tags[i + 1] as usize].clone();
                    }
                    i += 2;
                }
                no_such_key()
            }
        }
    }

    /// tippecanoe-rs's evaluator on `filter`, under `layer`.
    fn port(
        &self,
        filter: JsonRef<'_>,
        layer: &str,
        exclude_attributes: &mut BTreeSet<String>,
        unidecode_data: &[String],
    ) -> bool {
        match *self {
            Subject::Map(attrs) => {
                evaluator::evaluate(attrs, layer, filter, exclude_attributes, unidecode_data)
            }
            Subject::Tile { feat, layer, z } => evaluator::evaluate_feature(
                feat,
                layer,
                filter,
                exclude_attributes,
                z,
                unidecode_data,
            ),
        }
    }
}

/// The value tippecanoe-rs's getters return for an attribute that isn't there.
fn no_such_key() -> MvtValue {
    let mut v = MvtValue {
        type_: MVT_NO_SUCH_KEY,
        ..Default::default()
    };
    v.numeric_value.set_null_value(0);
    v
}

/// The error tippecanoe-rs's evaluator prints for `f`, with `f` as it prints it.
fn filter_error(prefix: &str, f: JsonRef<'_>) -> Error {
    let json = f.stringify_raw();
    let n = json.iter().position(|&c| c == 0).unwrap_or(json.len());
    Error::new(
        EXIT_FILTER,
        format!("{}{}", prefix, String::from_utf8_lossy(&json[..n])),
    )
}

/// The operation of `f`, if it is an expression of manby's.
fn own_op(f: JsonRef<'_>) -> Option<&str> {
    if f.type_() != JsonType::Array || f.array_len() == 0 {
        return None;
    }
    let op = f.array_get(0);
    if op.type_() != JsonType::String {
        return None;
    }
    match op.string() {
        op @ ("like" | "!like" | "ilike" | "!ilike" | "intersects") => Some(op),
        _ => None,
    }
}

/// Whether evaluating `f` can reach an expression of manby's: through the
/// combinators that tippecanoe evaluates the elements of. A combinator that
/// tippecanoe would reject before it got to them is left to tippecanoe.
fn has_own(f: JsonRef<'_>) -> bool {
    if own_op(f).is_some() {
        return true;
    }
    if f.type_() != JsonType::Array || f.array_len() == 0 {
        return false;
    }
    let a = f.array();
    if a[0].type_() != JsonType::String {
        return false;
    }
    match a[0].string() {
        "all" | "any" | "none" => a[1..].iter().any(|e| has_own(*e)),
        "attribute-filter" => a.len() == 3 && a[1].type_() == JsonType::String && has_own(a[2]),
        _ => false,
    }
}

/// Whether two JSON values are the same to tippecanoe's evaluator, which
/// takes numbers as doubles: -0 and 0 are the same.
fn same(a: JsonRef<'_>, b: JsonRef<'_>) -> bool {
    if a.type_() != b.type_() {
        return false;
    }
    match a.type_() {
        JsonType::String => a.raw_string_bytes() == b.raw_string_bytes(),
        JsonType::Number => {
            a.number() == b.number()
                && a.large_signed() == b.large_signed()
                && a.large_unsigned() == b.large_unsigned()
        }
        JsonType::Array => {
            a.array_len() == b.array_len()
                && a.array_iter().zip(b.array_iter()).all(|(x, y)| same(x, y))
        }
        JsonType::Hash => {
            let (ea, eb) = (a.entries(), b.entries());
            ea.len() == eb.len()
                && ea
                    .iter()
                    .zip(eb.iter())
                    .all(|(x, y)| same(x.0, y.0) && same(x.1, y.1))
        }
        _ => true,
    }
}

/// `f` as a filter of its own for tippecanoe-rs's evaluator: `{"*": f}`.
fn port_filter(f: JsonRef<'_>) -> Result<JsonTree> {
    let mut text = b"{\"*\":".to_vec();
    text.extend_from_slice(&f.stringify_raw());
    text.push(b'}');
    let mut jp = JsonPull::begin_string(&text);
    let tree = jp.read_tree();
    match tree {
        Some(tree) if tree.root().hash_get("*").is_some_and(|g| same(f, g)) => Ok(tree),
        _ => Err(filter_error("Filter can't be copied exactly: ", f)),
    }
}

impl Expr {
    fn compile(f: JsonRef<'_>, with_geometry: bool) -> Result<Expr> {
        if !has_own(f) {
            return Ok(Expr::Port(port_filter(f)?));
        }
        let a = f.array();
        let op = a[0].string();
        match op {
            "like" | "!like" | "ilike" | "!ilike" => {
                if a.len() != 3 {
                    return Err(filter_error(
                        "Wrong number of array elements in filter: ",
                        f,
                    ));
                }
                if a[1].type_() != JsonType::String || a[2].type_() != JsonType::String {
                    return Err(filter_error(
                        "\"like\" key and pattern must be strings: ",
                        f,
                    ));
                }
                let ignore_case = op.ends_with("ilike");
                Ok(Expr::Like {
                    key: a[1].string().to_string(),
                    pattern: if ignore_case {
                        a[2].string().to_lowercase()
                    } else {
                        a[2].string().to_string()
                    },
                    ignore_case,
                    negate: op.starts_with('!'),
                })
            }
            "intersects" => {
                if a.len() != 2 {
                    return Err(filter_error(
                        "Wrong number of array elements in filter: ",
                        f,
                    ));
                }
                if !with_geometry {
                    return Err(filter_error(
                        "\"intersects\" needs feature geometry, which only overzoom filters have: ",
                        f,
                    ));
                }
                Ok(Expr::Intersects(Area::from_geojson(a[1])?))
            }
            "all" | "any" | "none" => {
                let items = a[1..]
                    .iter()
                    .map(|e| Expr::compile(*e, with_geometry))
                    .collect::<Result<Vec<_>>>()?;
                Ok(match op {
                    "all" => Expr::All(items),
                    "any" => Expr::Any(items),
                    _ => Expr::None(items),
                })
            }
            // "attribute-filter", as has_own leaves no other
            _ => Ok(Expr::AttributeFilter(
                a[1].string().to_string(),
                Box::new(Expr::compile(a[2], with_geometry)?),
            )),
        }
    }

    /// Whether the feature passes, as tippecanoe's `eval` returns it. That is
    /// never -1 (sql null) for any expression, so a bool is enough.
    fn eval(
        &self,
        s: &Subject<'_>,
        exclude_attributes: &mut BTreeSet<String>,
        unidecode_data: &[String],
        geom: Option<&FilterGeometry<'_>>,
    ) -> bool {
        match self {
            Expr::Port(filter) => s.port(filter.root(), "", exclude_attributes, unidecode_data),
            Expr::All(items) => items
                .iter()
                .all(|e| e.eval(s, exclude_attributes, unidecode_data, geom)),
            Expr::Any(items) => items
                .iter()
                .any(|e| e.eval(s, exclude_attributes, unidecode_data, geom)),
            Expr::None(items) => !items
                .iter()
                .any(|e| e.eval(s, exclude_attributes, unidecode_data, geom)),
            Expr::AttributeFilter(key, e) => {
                if !e.eval(s, exclude_attributes, unidecode_data, geom) {
                    exclude_attributes.insert(key.clone());
                }
                true
            }
            Expr::Like {
                key,
                pattern,
                ignore_case,
                negate,
            } => {
                let Some(text) = value_text(&s.get(key)) else {
                    return *negate; // not found or null: like the other negations, true
                };
                let found = if *ignore_case {
                    like(&text.to_lowercase(), pattern)
                } else {
                    like(&text, pattern)
                };
                found != *negate
            }
            Expr::Intersects(area) => geom.is_some_and(|g| area.intersects(g)),
        }
    }
}

impl Filter {
    /// The filter `tree`. `with_geometry` says whether its features will have
    /// their geometry, which `intersects` needs: only overzoom's do.
    pub fn new(tree: JsonTree, with_geometry: bool) -> Result<Filter> {
        let root = tree.root();
        let mut exprs = None;
        if root.type_() == JsonType::Hash && root.entries_iter().any(|(_, f)| has_own(f)) {
            let mut map = HashMap::new();
            for (_, f) in root.entries_iter() {
                map.insert(f.id(), Expr::compile(f, with_geometry)?);
            }
            exprs = Some(map);
        }
        Ok(Filter(Arc::new(Inner { tree, exprs })))
    }

    /// The filter as it was parsed.
    pub fn tree(&self) -> &JsonTree {
        &self.0.tree
    }

    /// tippecanoe-rs's `evaluate`: whether the feature with the attributes
    /// `feature` in `layer` passes.
    pub fn evaluate(
        &self,
        feature: &HashMap<String, MvtValue>,
        layer: &str,
        exclude_attributes: &mut BTreeSet<String>,
        unidecode_data: &[String],
    ) -> bool {
        self.run(
            &Subject::Map(feature),
            layer,
            exclude_attributes,
            unidecode_data,
            None,
        )
    }

    /// tippecanoe-rs's `evaluate_feature`, with the feature's geometry for
    /// `intersects`.
    pub fn evaluate_feature(
        &self,
        feat: &MvtFeature,
        layer: &MvtLayer,
        exclude_attributes: &mut BTreeSet<String>,
        z: i32,
        unidecode_data: &[String],
        geom: Option<&FilterGeometry<'_>>,
    ) -> bool {
        self.run(
            &Subject::Tile { feat, layer, z },
            &layer.name,
            exclude_attributes,
            unidecode_data,
            geom,
        )
    }

    /// tippecanoe's `evaluate`: the layer's expression, then the one for every
    /// layer, "*".
    fn run(
        &self,
        s: &Subject<'_>,
        layer: &str,
        exclude_attributes: &mut BTreeSet<String>,
        unidecode_data: &[String],
        geom: Option<&FilterGeometry<'_>>,
    ) -> bool {
        let root = self.0.tree.root();
        let Some(exprs) = &self.0.exprs else {
            return s.port(root, layer, exclude_attributes, unidecode_data);
        };
        let mut ok = true;
        // looked up by its C string, as tippecanoe does
        if let Some(f) = root.hash_get(layer.split('\0').next().unwrap_or("")) {
            ok = exprs[&f.id()].eval(s, exclude_attributes, unidecode_data, geom);
        }
        if ok && let Some(f) = root.hash_get("*") {
            ok = exprs[&f.id()].eval(s, exclude_attributes, unidecode_data, geom);
        }
        ok
    }
}

/// The attribute as the text a SQL cast to VARCHAR gives, or None for null.
fn value_text(v: &MvtValue) -> Option<String> {
    match v.type_ {
        MVT_STRING => Some(v.get_str().to_string()),
        MVT_DOUBLE => Some(v.numeric_value.double_value().to_string()),
        MVT_FLOAT => Some(v.numeric_value.float_value().to_string()),
        MVT_INT => Some(v.numeric_value.int_value().to_string()),
        MVT_UINT => Some(v.numeric_value.uint_value().to_string()),
        MVT_SINT => Some(v.numeric_value.sint_value().to_string()),
        MVT_BOOL => Some(v.numeric_value.bool_value().to_string()),
        _ => None,
    }
}

/// SQL LIKE: % matches any run of characters, _ any one character. Only the
/// pattern has wildcards: a % or _ in the text is a plain character.
fn like(text: &str, pattern: &str) -> bool {
    let t: Vec<char> = text.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    let (mut ti, mut pi) = (0, 0);
    // where the last % was, and the text position it is matched up to
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        // % first, so that a % in the text cannot match it as a plain character
        if pi < p.len() && p[pi] == '%' {
            star = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && (p[pi] == '_' || p[pi] == t[ti]) {
            ti += 1;
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '%')
}

type Pt = (i64, i64);

/// Twice the signed area of a, b, c: positive when c is left of a to b.
fn orient(a: Pt, b: Pt, c: Pt) -> i128 {
    (b.0 - a.0) as i128 * (c.1 - a.1) as i128 - (b.1 - a.1) as i128 * (c.0 - a.0) as i128
}

/// c, collinear with a and b, lies within their bounding box.
fn on_segment(a: Pt, b: Pt, c: Pt) -> bool {
    a.0.min(b.0) <= c.0 && c.0 <= a.0.max(b.0) && a.1.min(b.1) <= c.1 && c.1 <= a.1.max(b.1)
}

fn segments_intersect(p1: Pt, p2: Pt, q1: Pt, q2: Pt) -> bool {
    let d1 = orient(q1, q2, p1);
    let d2 = orient(q1, q2, p2);
    let d3 = orient(p1, p2, q1);
    let d4 = orient(p1, p2, q2);
    if ((d1 > 0 && d2 < 0) || (d1 < 0 && d2 > 0)) && ((d3 > 0 && d4 < 0) || (d3 < 0 && d4 > 0)) {
        return true;
    }
    (d1 == 0 && on_segment(q1, q2, p1))
        || (d2 == 0 && on_segment(q1, q2, p2))
        || (d3 == 0 && on_segment(p1, p2, q1))
        || (d4 == 0 && on_segment(p1, p2, q2))
}

/// Whether p is inside rings that are filled even-odd (outer rings and holes).
fn inside_rings(rings: &[Vec<Pt>], p: Pt) -> bool {
    let mut inside = false;
    for ring in rings {
        for i in 0..ring.len() {
            if crosses_ray(ring[i], ring[(i + 1) % ring.len()], p) {
                inside = !inside;
            }
        }
    }
    inside
}

/// The edge a to b crosses the ray from p towards +x.
fn crosses_ray(a: Pt, b: Pt, p: Pt) -> bool {
    let (lo, hi) = if a.1 <= b.1 { (a, b) } else { (b, a) };
    lo.1 <= p.1 && p.1 < hi.1 && orient(lo, hi, p) > 0
}

/// A filter area in world coordinates, its edges bucketed into horizontal rows
/// so that a point or segment is only tested against the edges near it.
#[derive(Debug)]
struct Area {
    rings: Vec<Vec<Pt>>,
    edges: Vec<(Pt, Pt)>,
    min: Pt,
    max: Pt,
    rows: Vec<Vec<u32>>,
    row_height: i64,
}

impl Area {
    fn from_geojson(g: JsonRef<'_>) -> Result<Area> {
        let fail = |msg: &str| filter_error(msg, g);
        if g.type_() != JsonType::Hash {
            return Err(fail(
                "\"intersects\" needs a GeoJSON Polygon or MultiPolygon: ",
            ));
        }
        let kind = g
            .hash_get("type")
            .filter(|t| t.type_() == JsonType::String)
            .map(|t| t.string());
        let Some(coords) = g
            .hash_get("coordinates")
            .filter(|c| c.type_() == JsonType::Array)
        else {
            return Err(fail("\"intersects\" geometry has no coordinates: "));
        };
        let polygons = match kind {
            Some("Polygon") => vec![coords],
            Some("MultiPolygon") => coords.array(),
            _ => {
                return Err(fail(
                    "\"intersects\" needs a GeoJSON Polygon or MultiPolygon: ",
                ));
            }
        };

        let not_lonlat = || fail("\"intersects\" coordinate is not [longitude, latitude]: ");
        let mut rings = Vec::new();
        for polygon in polygons {
            if polygon.type_() != JsonType::Array {
                return Err(not_lonlat());
            }
            for ring in polygon.array() {
                if ring.type_() != JsonType::Array {
                    return Err(not_lonlat());
                }
                let mut lonlat: Vec<(f64, f64)> = Vec::new();
                for c in ring.array() {
                    if c.type_() != JsonType::Array {
                        return Err(not_lonlat());
                    }
                    let xy = c.array();
                    if xy.len() < 2
                        || xy[0].type_() != JsonType::Number
                        || xy[1].type_() != JsonType::Number
                    {
                        return Err(not_lonlat());
                    }
                    lonlat.push((xy[0].number(), xy[1].number()));
                }
                // Edges are straight in longitude and latitude, as SQL's ST_Intersects
                // treats them, but would be straight in Web Mercator once projected:
                // a 20 km edge would bend by about a metre. Points every 0.01 degree
                // keep that to millimetres.
                let mut pts: Vec<Pt> = Vec::new();
                for (i, &(lon, lat)) in lonlat.iter().enumerate() {
                    if i > 0 {
                        let (lon0, lat0) = lonlat[i - 1];
                        let steps =
                            ((lon - lon0).abs().max((lat - lat0).abs()) / 0.01).ceil() as usize;
                        for k in 1..steps {
                            let t = k as f64 / steps as f64;
                            pts.push(lonlat2tile(
                                lon0 + (lon - lon0) * t,
                                lat0 + (lat - lat0) * t,
                                32,
                            ));
                        }
                    }
                    pts.push(lonlat2tile(lon, lat, 32));
                }
                if pts.len() > 1 && pts[0] == pts[pts.len() - 1] {
                    pts.pop();
                }
                if pts.len() >= 3 {
                    rings.push(pts);
                }
            }
        }
        if rings.is_empty() {
            return Err(fail("\"intersects\" polygon has no ring: "));
        }

        let mut edges = Vec::new();
        let (mut min, mut max) = ((i64::MAX, i64::MAX), (i64::MIN, i64::MIN));
        for ring in &rings {
            for i in 0..ring.len() {
                let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
                edges.push((a, b));
                min = (min.0.min(a.0), min.1.min(a.1));
                max = (max.0.max(a.0), max.1.max(a.1));
            }
        }

        // about four edges to a row
        let nrows = (edges.len() / 4).clamp(1, 4096);
        let row_height = ((max.1 - min.1) / nrows as i64).max(1);
        let mut rows = vec![Vec::new(); nrows];
        for (i, (a, b)) in edges.iter().enumerate() {
            let (r0, r1) = (
                Self::row(min, row_height, nrows, a.1.min(b.1)),
                Self::row(min, row_height, nrows, a.1.max(b.1)),
            );
            for row in &mut rows[r0..=r1] {
                row.push(i as u32);
            }
        }
        Ok(Area {
            rings,
            edges,
            min,
            max,
            rows,
            row_height,
        })
    }

    fn row(min: Pt, row_height: i64, nrows: usize, y: i64) -> usize {
        (((y - min.1) / row_height).max(0) as usize).min(nrows - 1)
    }

    fn contains(&self, p: Pt) -> bool {
        if p.0 < self.min.0 || p.0 > self.max.0 || p.1 < self.min.1 || p.1 > self.max.1 {
            return false;
        }
        let r = Self::row(self.min, self.row_height, self.rows.len(), p.1);
        let mut inside = false;
        for &e in &self.rows[r] {
            let (a, b) = self.edges[e as usize];
            if crosses_ray(a, b, p) {
                inside = !inside;
            }
        }
        inside
    }

    /// The segment touches the area: an end inside, or crossing an edge.
    fn touches_segment(&self, p: Pt, q: Pt) -> bool {
        if p.0.max(q.0) < self.min.0
            || p.0.min(q.0) > self.max.0
            || p.1.max(q.1) < self.min.1
            || p.1.min(q.1) > self.max.1
        {
            return false;
        }
        if self.contains(p) || self.contains(q) {
            return true;
        }
        let nrows = self.rows.len();
        let (r0, r1) = (
            Self::row(self.min, self.row_height, nrows, p.1.min(q.1)),
            Self::row(self.min, self.row_height, nrows, p.1.max(q.1)),
        );
        self.rows[r0..=r1].iter().flatten().any(|&e| {
            let (a, b) = self.edges[e as usize];
            segments_intersect(p, q, a, b)
        })
    }

    fn intersects(&self, g: &FilterGeometry<'_>) -> bool {
        // the parts of the geometry, each a point, a line or a polygon ring
        let mut parts: Vec<Vec<Pt>> = Vec::new();
        for d in g.draws {
            let p = (d.x + g.origin.0, d.y + g.origin.1);
            if d.op == VT_MOVETO {
                parts.push(vec![p]);
            } else if d.op == VT_LINETO {
                match parts.last_mut() {
                    Some(part) => part.push(p),
                    None => parts.push(vec![p]),
                }
            }
        }

        match g.type_ {
            VT_POINT => parts.iter().flatten().any(|&p| self.contains(p)),
            // a single vertex is not a line: clipping to a tile leaves the vertices it cut off as lone moves
            VT_LINE => parts
                .iter()
                .any(|part| part.windows(2).any(|w| self.touches_segment(w[0], w[1]))),
            VT_POLYGON => {
                let crossing = parts.iter().any(|ring| {
                    (0..ring.len())
                        .any(|i| self.touches_segment(ring[i], ring[(i + 1) % ring.len()]))
                });
                // or a part of the area lies wholly inside the polygon. With no
                // edges crossing, each of the area's rings is wholly inside it or
                // wholly outside, so one vertex of each ring tells; only those
                // within the polygon's bounding box can be inside.
                let (mut lo, mut hi) = ((i64::MAX, i64::MAX), (i64::MIN, i64::MIN));
                for &p in parts.iter().flatten() {
                    lo = (lo.0.min(p.0), lo.1.min(p.1));
                    hi = (hi.0.max(p.0), hi.1.max(p.1));
                }
                crossing
                    || self.rings.iter().any(|ring| {
                        let p = ring[0];
                        lo.0 <= p.0
                            && p.0 <= hi.0
                            && lo.1 <= p.1
                            && p.1 <= hi.1
                            && inside_rings(&parts, p)
                    })
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tippecanoe::evaluator::parse_filter;
    use tippecanoe::mvt::stringified_to_mvt_value;

    fn filter(s: &str) -> Filter {
        Filter::new(parse_filter(s), true).unwrap()
    }

    /// The filter on a feature with no attributes and the geometry `geom`.
    fn on_geometry(f: &Filter, geom: &FilterGeometry<'_>) -> bool {
        let attrs = HashMap::new();
        f.run(
            &Subject::Map(&attrs),
            "l",
            &mut BTreeSet::new(),
            &[],
            Some(geom),
        )
    }

    #[test]
    fn like_matches_sql() {
        assert!(like("Lidl Berlin", "Lidl%"));
        assert!(like("Lidl Berlin", "%Berlin"));
        assert!(like("Lidl Berlin", "%dl B%"));
        assert!(like("abc", "a_c"));
        assert!(!like("abc", "a_"));
        assert!(like("", "%"));
        assert!(!like("Lidl", "lidl"));
        assert!(like("aXbXc", "%X%c"));
        assert!(!like("aXbXd", "%X%c"));
        assert!(like("Straße", "%ß%"));
    }

    // % and _ are wildcards only in the pattern; in the text they are plain
    // characters. Expected values are DuckDB's LIKE without ESCAPE, which also
    // takes a backslash as a plain character.
    #[test]
    fn like_text_with_wildcard_characters_matches_sql() {
        assert!(like("a%b", "a%"));
        assert!(like("x%y%z", "x%z"));
        assert!(like("x%y%z", "%y%"));
        assert!(like("50%x", "50%"));
        assert!(like(r"50%[x](y)\z", "50%"));
        assert!(like("Rate 50%", "%50%"));
        assert!(like("a_b", "a_%"));
        assert!(like(r"50%[x](y)\z", r"%\z"));
        assert!(like(r"50%[x](y)\z", "%[x](y)%"));
        assert!(like(r"a\b", r"a\%"));
        assert!(!like("a%", r"a\%"));
        assert!(like(r"a\bc", r"a\_c"));
        assert!(!like("abc", r"a\_c"));
        assert!(!like("a_c", r"a\_c"));
        assert!(like("[", "["));
        assert!(like("(", "("));
        assert!(!like("a", "."));
        assert!(like("a", "_"));
        assert!(like("%", "%%"));
        assert!(!like("x%y", "x%z"));
        assert!(like(&"Ärger".to_lowercase(), &"ä%".to_lowercase()));
    }

    #[test]
    fn like_filters() {
        let mut attrs = HashMap::new();
        attrs.insert(
            "name".to_string(),
            stringified_to_mvt_value(MVT_STRING, "Lidl Berlin"),
        );
        attrs.insert("n".to_string(), stringified_to_mvt_value(MVT_DOUBLE, "105"));
        attrs.insert(
            "rate".to_string(),
            stringified_to_mvt_value(MVT_STRING, "A%b"),
        );
        let cases: &[(&str, bool)] = &[
            (r#"{"*": ["ilike", "rate", "a%"]}"#, true),
            (r#"{"*": ["!ilike", "rate", "a%"]}"#, false),
            (r#"{"*": ["like", "rate", "a%"]}"#, false),
            (r#"{"*": ["like", "name", "Lidl%"]}"#, true),
            (r#"{"*": ["like", "name", "lidl%"]}"#, false),
            (r#"{"*": ["ilike", "name", "lidl%"]}"#, true),
            (r#"{"*": ["!ilike", "name", "%aldi%"]}"#, true),
            (r#"{"*": ["!like", "name", "%Berlin"]}"#, false),
            (r#"{"*": ["ilike", "n", "10%"]}"#, true),
            (r#"{"*": ["ilike", "missing", "%"]}"#, false),
            (r#"{"*": ["!ilike", "missing", "%"]}"#, true),
            (
                r#"{"*": ["all", ["ilike", "name", "%berlin"], ["==", "n", 105]]}"#,
                true,
            ),
        ];
        for &(f, expected) in cases {
            let mut exclude = BTreeSet::new();
            assert_eq!(
                filter(f).evaluate(&attrs, "l", &mut exclude, &[]),
                expected,
                "{f}"
            );
        }
    }

    /// The feature of tippecanoe-rs's `filters_match_the_cpp`: n=5, s="abc"
    /// and b=false in layer "layer".
    fn cpp_feature() -> HashMap<String, MvtValue> {
        let mut attrs = HashMap::new();
        attrs.insert("n".to_string(), stringified_to_mvt_value(MVT_DOUBLE, "5"));
        attrs.insert("s".to_string(), stringified_to_mvt_value(MVT_STRING, "abc"));
        attrs.insert("b".to_string(), stringified_to_mvt_value(MVT_BOOL, "false"));
        attrs
    }

    /// tippecanoe-rs's cases, which it evaluates itself, and again with a
    /// `like` that always matches beside them, so that manby walks the
    /// expressions and hands the cases over one at a time.
    #[test]
    fn expressions_without_like_are_tippecanoes() {
        let attrs = cpp_feature();
        let cases: &[(&str, bool, &[&str])] = &[
            (r#"["==", "n", 5]"#, true, &[]),
            (r#"["<", "s", "abd"]"#, true, &[]),
            (r#"[">", "b", true]"#, false, &[]),
            (r#"["==", "b", true]"#, true, &[]),
            (r#"["in", "n", 1, 2, 5]"#, true, &[]),
            (r#"["!in", "n", "x", 5]"#, false, &[]),
            (r#"["has", "n"]"#, true, &[]),
            (r#"["!has", "zz"]"#, true, &[]),
            (r#"["any", ["==", "n", 4], ["==", "s", "abc"]]"#, true, &[]),
            (r#"["none", ["==", "n", 4]]"#, true, &[]),
            (
                r#"["all", ["attribute-filter", "s", ["==", "n", 4]], true]"#,
                true,
                &["s"],
            ),
            (r#"0"#, false, &[]),
            (r#""x""#, true, &[]),
            (r#"null"#, true, &[]),
            (r#"["==", "n", 5.000000000000001]"#, false, &[]),
            (r#"["<", "n", 5.000000000000001]"#, true, &[]),
            (r#"["<", "n", 1e21]"#, true, &[]),
            (r#"[">", "n", -9007199254740993]"#, true, &[]),
        ];
        for &(expr, expected, excluded) in cases {
            let excluded: BTreeSet<String> = excluded.iter().map(|s| s.to_string()).collect();
            for f in [
                format!(r#"{{"layer": {expr}}}"#),
                format!(r#"{{"layer": ["all", ["like", "s", "%"], {expr}]}}"#),
                format!(r#"{{"*": ["any", ["!like", "s", "%"], {expr}]}}"#),
            ] {
                let f = filter(&f);
                let mut exclude = BTreeSet::new();
                assert_eq!(
                    f.evaluate(&attrs, "layer", &mut exclude, &[]),
                    expected,
                    "{expr}"
                );
                assert_eq!(exclude, excluded, "{expr}");
            }
        }
    }

    #[test]
    fn a_filter_without_like_is_left_to_tippecanoe() {
        assert!(
            filter(r#"{"*": ["all", ["==", "n", 5], ["has", "s"]]}"#)
                .0
                .exprs
                .is_none()
        );
        // like as a value to compare with is not an expression
        assert!(
            filter(r#"{"*": ["==", "s", ["like", "s", "%"]]}"#)
                .0
                .exprs
                .is_none()
        );
        assert!(
            filter(r#"{"*": ["any", ["like", "s", "%"]]}"#)
                .0
                .exprs
                .is_some()
        );
    }

    #[test]
    fn like_inside_combinators() {
        let attrs = cpp_feature();
        let cases: &[(&str, bool, &[&str])] = &[
            (r#"["any", ["==", "n", 4], ["like", "s", "a%"]]"#, true, &[]),
            (r#"["none", ["like", "s", "a%"]]"#, false, &[]),
            (
                r#"["none", ["ilike", "s", "X%"], ["==", "n", 4]]"#,
                true,
                &[],
            ),
            (
                r#"["all", ["any", ["none", ["!like", "s", "_b_"]], false], ["in", "n", 5]]"#,
                true,
                &[],
            ),
            (
                r#"["all", ["any", ["none", ["like", "s", "_b_"]], false], ["in", "n", 5]]"#,
                false,
                &[],
            ),
            (r#"["all"]"#, true, &[]),
            (r#"["any", ["like", "zz", "%"]]"#, false, &[]),
            // attribute-filter keeps the feature, and drops the attribute
            // when its expression is false
            (
                r#"["attribute-filter", "s", ["like", "s", "x%"]]"#,
                true,
                &["s"],
            ),
            (
                r#"["all", ["attribute-filter", "s", ["like", "s", "a%"]], ["attribute-filter", "n", ["ilike", "s", "%Z"]]]"#,
                true,
                &["n"],
            ),
            // all stops at the first false, so the attribute-filter after it
            // is never reached
            (
                r#"["all", ["like", "s", "x%"], ["attribute-filter", "n", false]]"#,
                false,
                &[],
            ),
            // any stops at the first true
            (
                r#"["any", ["like", "s", "a%"], ["attribute-filter", "n", false]]"#,
                true,
                &[],
            ),
            (
                r#"["any", ["like", "s", "x%"], ["attribute-filter", "n", false]]"#,
                true,
                &["n"],
            ),
        ];
        for &(expr, expected, excluded) in cases {
            let f = filter(&format!(r#"{{"layer": {expr}}}"#));
            let mut exclude = BTreeSet::new();
            assert_eq!(
                f.evaluate(&attrs, "layer", &mut exclude, &[]),
                expected,
                "{expr}"
            );
            let excluded: BTreeSet<String> = excluded.iter().map(|s| s.to_string()).collect();
            assert_eq!(exclude, excluded, "{expr}");
        }
    }

    #[test]
    fn the_layers_expression_then_every_layers() {
        let attrs = cpp_feature();
        let check =
            |f: &str, layer: &str| filter(f).evaluate(&attrs, layer, &mut BTreeSet::new(), &[]);
        let f = r#"{"layer": ["like", "s", "a%"], "*": ["==", "n", 5]}"#;
        assert!(check(f, "layer"));
        assert!(check(f, "other"));
        let f = r#"{"layer": ["like", "s", "x%"], "*": ["==", "n", 5]}"#;
        assert!(!check(f, "layer"));
        assert!(check(f, "other"));
        let f = r#"{"layer": ["==", "n", 5], "*": ["!like", "s", "a%"]}"#;
        assert!(!check(f, "layer"));
        // the first of two expressions for a layer
        let f = r#"{"layer": ["like", "s", "a%"], "layer": false}"#;
        assert!(check(f, "layer"));
        // layer names are looked up up to a NUL
        assert!(!check(r#"{"layer": ["like", "s", "x%"]}"#, "layer\0more"));
    }

    #[test]
    fn feature_getters() {
        let mut layer = MvtLayer {
            name: "l".to_string(),
            ..Default::default()
        };
        let mut feat = MvtFeature {
            type_: MVT_LINESTRING,
            id: 1234,
            has_id: true,
            ..Default::default()
        };
        layer.tag(&mut feat, "k", &stringified_to_mvt_value(MVT_STRING, "v"));
        let check = |f: &str, z: i32| {
            filter(f).evaluate_feature(&feat, &layer, &mut BTreeSet::new(), z, &[], None)
        };
        assert!(check(r#"{"l": ["like", "k", "v"]}"#, 0));
        assert!(check(r#"{"l": ["like", "$id", "12%"]}"#, 0));
        assert!(check(r#"{"l": ["ilike", "$type", "line%"]}"#, 0));
        assert!(check(r#"{"*": ["like", "$zoom", "1_"]}"#, 14));
        assert!(!check(r#"{"*": ["like", "$zoom", "1_"]}"#, 9));
        assert!(check(
            r#"{"*": ["all", ["like", "k", "%"], ["==", "$zoom", 9]]}"#,
            9
        ));
    }

    #[test]
    fn errors() {
        let error = |f: &str, with_geometry: bool| {
            Filter::new(parse_filter(f), with_geometry)
                .err()
                .map(|e| (e.exit_code, e.message))
        };
        assert_eq!(
            error(r#"{"*": ["like", "s"]}"#, true),
            Some((
                EXIT_FILTER,
                r#"Wrong number of array elements in filter: ["like","s"]"#.to_string()
            ))
        );
        assert_eq!(
            error(r#"{"*": ["any", ["ilike", "s", 5]]}"#, true),
            Some((
                EXIT_FILTER,
                r#""like" key and pattern must be strings: ["ilike","s",5]"#.to_string()
            ))
        );
        let square = r#"{"type": "Polygon", "coordinates": [[[0, 0], [1, 0], [1, 1], [0, 0]]]}"#;
        assert!(error(&format!(r#"{{"*": ["intersects", {square}]}}"#), true).is_none());
        assert_eq!(
            error(&format!(r#"{{"*": ["intersects", {square}]}}"#), false)
                .unwrap()
                .1,
            r#""intersects" needs feature geometry, which only overzoom filters have: ["intersects",{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}]"#
        );
        assert_eq!(
            error(
                r#"{"*": ["intersects", {"type": "Point", "coordinates": [0, 0]}]}"#,
                true
            )
            .unwrap()
            .1,
            r#""intersects" needs a GeoJSON Polygon or MultiPolygon: {"type":"Point","coordinates":[0,0]}"#
        );
        assert_eq!(
            error(
                r#"{"*": ["intersects", {"type": "Polygon", "coordinates": [[0, 0]]}]}"#,
                true
            )
            .unwrap()
            .1,
            r#""intersects" coordinate is not [longitude, latitude]: {"type":"Polygon","coordinates":[[0,0]]}"#
        );
        assert_eq!(
            error(r#"{"*": ["intersects", {"type": "Polygon"}]}"#, true)
                .unwrap()
                .1,
            r#""intersects" geometry has no coordinates: {"type":"Polygon"}"#
        );
        assert_eq!(
            error(
                r#"{"*": ["intersects", {"type": "Polygon", "coordinates": [[[0, 0], [0, 0]]]}]}"#,
                true
            )
            .unwrap()
            .1,
            r#""intersects" polygon has no ring: {"type":"Polygon","coordinates":[[[0,0],[0,0]]]}"#
        );
        // tippecanoe's own operators with errors are left to tippecanoe
        assert!(error(r#"{"*": ["all", ["like", "s", "%"], ["bogus"]]}"#, true).is_none());
    }

    #[test]
    fn expressions_are_handed_over_exactly() {
        for expr in [
            r#"["in", "n", 0.1, 1e21, 5e-324, 1.7976931348623157e308, -0.0, 123456789012345678]"#,
            r#"["in", "n", 18446744073709551615, -9223372036854775808, 9007199254740993]"#,
            r#"["in", "s", "a\"b\\c", "\u0000x\u001f", "ß", "😀", ""]"#,
            r#"["all", true, false, null, {"a": [1, {"b": null}]}]"#,
        ] {
            let tree = parse_filter(expr);
            let copy = port_filter(tree.root()).unwrap();
            assert!(
                same(tree.root(), copy.root().hash_get("*").unwrap()),
                "{expr}"
            );
        }
        assert!(!same(
            parse_filter("[1]").root(),
            parse_filter("[1.0000000000000002]").root()
        ));
    }

    #[test]
    fn intersects_points_lines_and_polygons() {
        // a 1 x 1 degree square
        let f = filter(
            r#"{"*": ["intersects", {"type": "Polygon", "coordinates": [[[10, 10], [11, 10], [11, 11], [10, 11], [10, 10]]]}]}"#,
        );
        let at = |lon: f64, lat: f64| lonlat2tile(lon, lat, 32);
        let draw = |op: i8, p: (i64, i64), origin: (i64, i64)| Draw {
            x: p.0 - origin.0,
            op,
            y: p.1 - origin.1,
            necessary: 0,
        };
        let check = |type_: i32, parts: &[&[(f64, f64)]], origin: (i64, i64)| {
            let mut draws = Vec::new();
            for part in parts {
                for (i, &(lon, lat)) in part.iter().enumerate() {
                    draws.push(draw(
                        if i == 0 { VT_MOVETO } else { VT_LINETO },
                        at(lon, lat),
                        origin,
                    ));
                }
            }
            let geom = FilterGeometry {
                draws: &draws,
                origin,
                type_,
            };
            on_geometry(&f, &geom)
        };
        let o = (0, 0);
        let shifted = (1 << 31, 1 << 30); // the same, relative to a tile's origin
        assert!(check(VT_POINT, &[&[(10.5, 10.5)]], o));
        assert!(check(VT_POINT, &[&[(10.5, 10.5)]], shifted));
        assert!(!check(VT_POINT, &[&[(12.0, 12.0)]], o));
        assert!(check(VT_POINT, &[&[(12.0, 12.0)], &[(10.2, 10.8)]], o)); // multipoint, one inside
        // a line through the square with no vertex inside it
        assert!(check(VT_LINE, &[&[(9.0, 10.5), (12.0, 10.5)]], o));
        assert!(!check(VT_LINE, &[&[(9.0, 12.0), (12.0, 12.5)]], o));
        // clipping a line to a tile leaves the vertices it cut off as lone moves:
        // they are no longer part of the line, even inside the square
        assert!(!check(
            VT_LINE,
            &[&[(10.5, 10.5)], &[(9.0, 12.0), (12.0, 12.5)]],
            o
        ));
        // a polygon around the whole square, one beside it, one overlapping a corner
        assert!(check(
            VT_POLYGON,
            &[&[
                (0.0, 0.0),
                (20.0, 0.0),
                (20.0, 20.0),
                (0.0, 20.0),
                (0.0, 0.0)
            ]],
            o
        ));
        assert!(!check(
            VT_POLYGON,
            &[&[
                (12.0, 12.0),
                (13.0, 12.0),
                (13.0, 13.0),
                (12.0, 13.0),
                (12.0, 12.0)
            ]],
            o
        ));
        assert!(check(
            VT_POLYGON,
            &[&[
                (10.8, 10.8),
                (12.0, 10.8),
                (12.0, 12.0),
                (10.8, 12.0),
                (10.8, 10.8)
            ]],
            shifted
        ));
    }

    /// The filter on a point feature at `lon`, `lat`.
    fn on_point(f: &Filter, lon: f64, lat: f64) -> bool {
        let p = lonlat2tile(lon, lat, 32);
        let draws = [Draw::new(VT_MOVETO, p.0, p.1)];
        let geom = FilterGeometry {
            draws: &draws,
            origin: (0, 0),
            type_: VT_POINT,
        };
        on_geometry(f, &geom)
    }

    #[test]
    fn intersects_follows_straight_edges_in_longitude_and_latitude() {
        // the long sloped edge from (-71.10, 42.42) to (-71.19, 42.23): a point
        // halfway along it in degrees, nudged about 1.6 cm east and west
        let f = filter(
            r#"{"*": ["intersects", {"type": "Polygon", "coordinates": [[[-71.19, 42.23], [-70.99, 42.23], [-70.92, 42.40], [-71.10, 42.42], [-71.19, 42.23]]]}]}"#,
        );
        assert!(on_point(&f, -71.145 + 0.0000002, 42.325));
        assert!(!on_point(&f, -71.145 - 0.0000002, 42.325));
    }

    const TWO_PARTS: &str = r#"{"type": "MultiPolygon", "coordinates": [
        [[[0, 0], [4, 0], [4, 4], [0, 4], [0, 0]], [[1, 1], [3, 1], [3, 3], [1, 3], [1, 1]]],
        [[[10, 10], [11, 10], [11, 11], [10, 11], [10, 10]]]]}"#;

    #[test]
    fn intersects_respects_holes_and_multipolygons() {
        let f = filter(&format!(r#"{{"*": ["intersects", {TWO_PARTS}]}}"#));
        assert!(on_point(&f, 0.5, 0.5)); // in the ring
        assert!(!on_point(&f, 2.0, 2.0)); // in the hole
        assert!(on_point(&f, 10.5, 10.5)); // in the second polygon
        assert!(!on_point(&f, 5.0, 5.0));
    }

    #[test]
    fn intersects_a_polygon_around_any_part_of_a_multipolygon() {
        // the same two parts: a square with a hole, and a small square apart
        let f = filter(&format!(r#"{{"*": ["intersects", {TWO_PARTS}]}}"#));
        // a square polygon from (lo, lo) to (hi, hi), its edges touching no part
        let check = |lo: f64, hi: f64| {
            let corners = [(lo, lo), (hi, lo), (hi, hi), (lo, hi), (lo, lo)];
            let draws: Vec<Draw> = corners
                .iter()
                .enumerate()
                .map(|(i, &(lon, lat))| {
                    let p = lonlat2tile(lon, lat, 32);
                    Draw::new(if i == 0 { VT_MOVETO } else { VT_LINETO }, p.0, p.1)
                })
                .collect();
            let geom = FilterGeometry {
                draws: &draws,
                origin: (0, 0),
                type_: VT_POLYGON,
            };
            on_geometry(&f, &geom)
        };
        assert!(check(9.0, 12.0)); // around the second part only
        assert!(check(-1.0, 5.0)); // around the first part only
        assert!(check(-1.0, 12.0)); // around both
        assert!(!check(1.5, 2.5)); // in the hole of the first part
        assert!(!check(6.0, 8.0)); // between the parts
    }

    #[test]
    fn intersects_inside_combinators() {
        let f = filter(&format!(
            r#"{{"*": ["any", ["none", ["intersects", {TWO_PARTS}]], ["like", "name", "x%"]]}}"#
        ));
        assert!(!on_point(&f, 0.5, 0.5));
        assert!(on_point(&f, 5.0, 5.0));
        let f = filter(&format!(
            r#"{{"*": ["all", ["!has", "name"], ["intersects", {TWO_PARTS}]]}}"#
        ));
        assert!(on_point(&f, 10.5, 10.5));
        assert!(!on_point(&f, 2.0, 2.0));
    }
}
