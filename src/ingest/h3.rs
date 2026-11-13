//! `--h3`: below a base zoom, each layer's points are binned into H3
//! hexagons, a resolution for each zoom as freestiler picks them unless
//! `--h3-resolutions` gives them, and the hexagons, with how many points each
//! holds and what `--accumulate-attribute` and `--h3-median` make of their
//! attributes, go in a layer of their own, `<layer>_h3`. The points show from
//! the base zoom, or with `--h3-no-points` not at all.
//!
//! The points are counted as they are read, into their cells of each
//! resolution, each chunk on its own. A point's cell is found at each
//! resolution, as freestiler finds it, since a coarse cell does not hold all
//! of the finer cells that it is the parent of. Once every input is read,
//! the chunks' cells are added up in the order of the chunks, and the
//! hexagons go in as one more chunk, so tippecanoe's tiling and dropping
//! work on them as on any polygons.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};

use h3o::{CellIndex, LatLng, Resolution};
use tippecanoe::attribute::AttributeOp;
use tippecanoe::geometry::{Draw, DrawVec, VT_CLOSEPATH, VT_LINETO, VT_MOVETO};
use tippecanoe::milo::dtoa_milo;
use tippecanoe::mvt::{MVT_BOOL, MVT_DOUBLE, MVT_NULL, MVT_STRING};
use tippecanoe::projection::PROJECTIONS;
use tippecanoe::serial::SerialVal;

use super::serialize::InFeature;
use crate::options::Options;

/// The H3 resolution for a zoom, as freestiler maps them: hexagons a little
/// smaller than a tile's 256 pixels make them, and beyond zoom 14 seven
/// tenths of the zoom, rounded half to even as R rounds.
pub fn resolution(zoom: i32) -> u8 {
    match zoom {
        ..=1 => 1,
        2 | 3 => 2,
        4 => 3,
        5 => 4,
        6 | 7 => 5,
        8 => 6,
        9 | 10 => 7,
        11 => 8,
        12 | 13 => 9,
        14 => 10,
        z => (z as f64 * 0.7).round_ties_even().min(15.0) as u8,
    }
}

/// Category values kept by name; the rest count as `_other`.
const CATEGORIES: usize = 64;

/// What `--h3` needs, worked out from the options.
#[derive(Debug)]
pub struct Setup {
    /// The zoom from which the points show.
    pub base: i32,
    /// `--h3-no-points`: the points don't show.
    pub no_points: bool,
    /// `--h3-fade`: the zooms each resolution shows beyond its own.
    pub fade: i32,
    /// Each resolution's zooms: resolution, first zoom, last zoom.
    pub windows: Vec<(u8, i32, i32)>,
    /// The resolutions into which the points are counted.
    resolutions: Vec<Resolution>,
    /// The attributes to accumulate, and how.
    accumulate: Vec<(String, AttributeOp)>,
    /// `--h3-median`: the attributes whose median each hexagon has.
    median: Vec<String>,
    /// `--h3-categories`: the attribute whose values are counted.
    category: Option<String>,
}

impl Setup {
    pub fn new(o: &Options) -> Setup {
        let base = o.h3_base();
        let maxzoom = o.maxzoom.max(o.minzoom);
        let mut windows: Vec<(u8, i32, i32)> = Vec::new();
        for z in o.minzoom..base {
            let r = o
                .h3_resolutions
                .get(&z)
                .copied()
                .unwrap_or_else(|| resolution(z));
            match windows.last_mut() {
                Some(w) if w.0 == r => w.2 = z,
                _ => windows.push((r, z, z)),
            }
        }
        // --h3-fade: each resolution shows beyond its own zooms, to fade
        // into the next
        for w in windows.iter_mut() {
            w.1 = (w.1 - o.h3_fade).max(o.minzoom);
            w.2 = (w.2 + o.h3_fade).min(maxzoom);
        }
        let mut resolutions: Vec<Resolution> = windows
            .iter()
            .map(|w| Resolution::try_from(w.0).expect("a resolution of 0 to 15"))
            .collect();
        resolutions.sort();
        resolutions.dedup();
        let mut accumulate: Vec<(String, AttributeOp)> = o
            .attribute_accum
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        accumulate.sort_by(|a, b| a.0.cmp(&b.0));
        let mut median = o.h3_median.clone();
        median.sort();
        median.dedup();
        Setup {
            base: if o.h3_no_points {
                base
            } else {
                (base - o.h3_fade).max(o.minzoom)
            },
            no_points: o.h3_no_points,
            fade: o.h3_fade,
            windows,
            resolutions,
            accumulate,
            median,
            category: o.h3_categories.clone(),
        }
    }
}

impl Setup {
    /// The resolutions, their zooms, `--h3-fade` and the zoom from which the
    /// points show, as JSON, for `manby serve`'s map:
    /// `{"windows":[[resolution,minzoom,maxzoom],...],"fade":1,"points":10}`.
    pub fn json(&self) -> String {
        let windows: Vec<String> = self
            .windows
            .iter()
            .map(|w| format!("[{},{},{}]", w.0, w.1, w.2))
            .collect();
        format!(
            "{{\"windows\":[{}],\"fade\":{},\"points\":{}}}",
            windows.join(","),
            self.fade,
            if self.no_points {
                "null".to_string()
            } else {
                self.base.to_string()
            }
        )
    }
}

/// An attribute's points, added up as one of tippecanoe's accumulations.
#[derive(Clone, Debug, Default)]
struct Sum {
    /// The points that have the attribute.
    n: u64,
    sum: f64,
    product: f64,
    min: f64,
    max: f64,
}

impl Sum {
    fn add(&mut self, v: f64) {
        if self.n == 0 {
            *self = Sum {
                n: 1,
                sum: v,
                product: v,
                min: v,
                max: v,
            };
            return;
        }
        self.n += 1;
        self.sum += v;
        self.product *= v;
        self.min = self.min.min(v);
        self.max = self.max.max(v);
    }

    fn merge(&mut self, o: &Sum) {
        if o.n == 0 {
            return;
        }
        if self.n == 0 {
            *self = o.clone();
            return;
        }
        self.n += o.n;
        self.sum += o.sum;
        self.product *= o.product;
        self.min = self.min.min(o.min);
        self.max = self.max.max(o.max);
    }

    fn value(&self, op: AttributeOp) -> Option<f64> {
        if self.n == 0 {
            return None;
        }
        Some(match op {
            AttributeOp::Sum => self.sum,
            AttributeOp::Product => self.product,
            AttributeOp::Mean => self.sum / self.n as f64,
            AttributeOp::Max => self.max,
            AttributeOp::Min => self.min,
            AttributeOp::Count => self.n as f64,
            // strings of every point of a hexagon would be too long to keep
            AttributeOp::Concat | AttributeOp::Comma => return None,
        })
    }
}

/// The points of one cell.
#[derive(Clone, Debug, Default)]
struct Bin {
    count: u64,
    sums: Vec<Sum>,
    /// `--h3-median`: each attribute's values.
    values: Vec<Vec<f64>>,
    /// `--h3-categories`: the points of each value, and of none.
    categories: BTreeMap<String, u64>,
    uncategorized: u64,
}

impl Bin {
    fn merge(&mut self, o: &Bin) {
        self.count += o.count;
        if self.sums.len() < o.sums.len() {
            self.sums.resize(o.sums.len(), Sum::default());
        }
        for (a, b) in self.sums.iter_mut().zip(&o.sums) {
            a.merge(b);
        }
        if self.values.len() < o.values.len() {
            self.values.resize(o.values.len(), Vec::new());
        }
        for (a, b) in self.values.iter_mut().zip(&o.values) {
            a.extend_from_slice(b);
        }
        for (k, v) in &o.categories {
            *self.categories.entry(k.clone()).or_default() += v;
        }
        self.uncategorized += o.uncategorized;
    }
}

/// The median of some values, the mean of the middle two of an even number
/// of them, as DuckDB's `MEDIAN` makes it.
fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable_by(f64::total_cmp);
    let n = values.len();
    Some(if n % 2 == 1 {
        values[n / 2]
    } else {
        let (a, b) = (values[n / 2 - 1], values[n / 2]);
        a + (b - a) / 2.0
    })
}

/// The points one chunk counted, by layer and cell.
#[derive(Debug, Default)]
pub struct Bins {
    layers: BTreeMap<String, HashMap<u64, Bin>>,
}

/// The number an attribute's value stands for, or None for one that isn't
/// a number.
fn number(v: &SerialVal) -> Option<f64> {
    if v.type_ == MVT_STRING {
        return None;
    }
    let s = v.s.split('\0').next().unwrap_or("");
    s.parse::<f64>().ok()
}

impl Bins {
    /// Counts the points of a feature, at world coordinates `points`, with
    /// its attributes.
    pub fn add(
        &mut self,
        setup: &Setup,
        layer: &str,
        points: impl Iterator<Item = (i64, i64)>,
        keys: &[Cow<'_, str>],
        values: &[SerialVal],
    ) {
        // what the feature adds to each of its points' cells
        let mut point = Bin {
            count: 1,
            sums: vec![Sum::default(); setup.accumulate.len()],
            values: vec![Vec::new(); setup.median.len()],
            ..Bin::default()
        };
        let mut categorized = false;
        for (k, v) in keys.iter().zip(values) {
            if let Ok(i) = setup.accumulate.binary_search_by(|a| a.0.as_str().cmp(k))
                && let Some(n) = number(v)
            {
                point.sums[i].add(n);
            }
            if let Ok(i) = setup.median.binary_search_by(|a| a.as_str().cmp(k))
                && let Some(n) = number(v)
            {
                point.values[i].push(n);
            }
            if setup.category.as_deref() == Some(k.as_ref()) && v.type_ != MVT_NULL {
                let value = v.s.split('\0').next().unwrap_or("").to_string();
                point.categories.insert(value, 1);
                categorized = true;
            }
        }
        if setup.category.is_some() && !categorized {
            point.uncategorized = 1;
        }

        let cells = self.layers.entry(layer.to_string()).or_default();
        for (x, y) in points {
            let (lon, lat) = tippecanoe::projection::tile2lonlat(x, y, 32);
            let Ok(ll) = LatLng::new(lat, lon) else {
                continue;
            };
            for &r in &setup.resolutions {
                cells
                    .entry(u64::from(ll.to_cell(r)))
                    .or_default()
                    .merge(&point);
            }
        }
    }
}

/// A hexagon, ready to serialize.
pub struct Hexagon {
    pub layer: String,
    pub feature: InFeature<'static>,
}

/// The world coordinates of a cell's outline, as a polygon: split where it
/// crosses the antimeridian, and around a pole filled to the edge of the
/// Web Mercator world.
fn outline(cell: CellIndex) -> DrawVec {
    let project = PROJECTIONS[0].project;
    let mut ring: Vec<(f64, f64)> = cell
        .boundary()
        .iter()
        .map(|ll| (ll.lng(), ll.lat()))
        .collect();
    let mut dv = DrawVec::new();
    let draw_ring = |ring: &[(f64, f64)], dv: &mut DrawVec| {
        for (i, &(lon, lat)) in ring.iter().chain(ring.first()).enumerate() {
            let (x, y) = project(lon, lat, 32);
            dv.push(Draw::new(if i == 0 { VT_MOVETO } else { VT_LINETO }, x, y));
        }
        dv.push(Draw::new(VT_CLOSEPATH, 0, 0));
    };
    let pole = [90.0, -90.0]
        .into_iter()
        .find(|&lat| LatLng::new(lat, 0.0).is_ok_and(|ll| ll.to_cell(cell.resolution()) == cell));
    if let Some(lat) = pole {
        // the vertices around the pole, west to east, closed along the edge
        let edge = lat.signum() * 85.0511287798066;
        // what is beyond the edge of the Web Mercator world doesn't show
        for p in ring.iter_mut() {
            p.1 = p.1.clamp(-85.0511287798066, 85.0511287798066);
        }
        ring.sort_by(|a, b| a.0.total_cmp(&b.0));
        ring.insert(0, (-180.0, ring[0].1));
        ring.push((180.0, ring[ring.len() - 1].1));
        ring.push((180.0, edge));
        ring.push((-180.0, edge));
        draw_ring(&ring, &mut dv);
        return dv;
    }
    let (min, max) = ring
        .iter()
        .fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.0), b.max(p.0)));
    if max - min > 180.0 {
        // across the antimeridian: once east of it, once west
        let east: Vec<(f64, f64)> = ring
            .iter()
            .map(|&(lon, lat)| (if lon < 0.0 { lon + 360.0 } else { lon }, lat))
            .collect();
        let west: Vec<(f64, f64)> = east.iter().map(|&(lon, lat)| (lon - 360.0, lat)).collect();
        draw_ring(&east, &mut dv);
        draw_ring(&west, &mut dv);
    } else {
        draw_ring(&ring, &mut dv);
    }
    dv
}

fn string(s: &str) -> SerialVal {
    SerialVal::new(MVT_STRING, s)
}

fn double(v: f64) -> SerialVal {
    SerialVal::new(MVT_DOUBLE, &dtoa_milo(v))
}

fn boolean(v: bool) -> SerialVal {
    SerialVal::new(MVT_BOOL, if v { "true" } else { "false" })
}

/// The hexagons of the bins of every chunk, added up in the order given.
pub fn hexagons(setup: &Setup, chunks: Vec<Bins>) -> Vec<Hexagon> {
    let mut layers: BTreeMap<String, HashMap<u64, Bin>> = BTreeMap::new();
    for chunk in chunks {
        for (layer, cells) in chunk.layers {
            let into = layers.entry(layer).or_default();
            let mut cells: Vec<(u64, Bin)> = cells.into_iter().collect();
            cells.sort_unstable_by_key(|c| c.0);
            for (cell, bin) in cells {
                into.entry(cell).or_default().merge(&bin);
            }
        }
    }

    let mut out = Vec::new();
    for (layer, cells) in layers {
        // the category values kept by name: the most common, by count and name
        let mut totals: BTreeMap<&str, u64> = BTreeMap::new();
        for bin in cells.values() {
            for (k, v) in &bin.categories {
                *totals.entry(k.as_str()).or_default() += v;
            }
        }
        // a value of "_other" is counted with the others
        totals.remove("_other");
        let mut ranked: Vec<(&str, u64)> = totals.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let named: Vec<String> = ranked
            .iter()
            .take(if ranked.len() > CATEGORIES {
                CATEGORIES - 1
            } else {
                CATEGORIES
            })
            .map(|r| r.0.to_string())
            .collect();

        let mut cells: Vec<(CellIndex, Bin)> = cells
            .into_iter()
            .filter_map(|(cell, bin)| Some((CellIndex::try_from(cell).ok()?, bin)))
            .collect();
        cells.sort_unstable_by_key(|c| c.0);
        for &(res, minzoom, maxzoom) in &setup.windows {
            for (c, bin) in cells.iter_mut() {
                if u8::from(c.resolution()) != res {
                    continue;
                }
                let c = *c;
                let mut keys: Vec<Cow<'static, str>> = vec![
                    Cow::Borrowed("h3"),
                    Cow::Borrowed("h3_resolution"),
                    Cow::Borrowed("point_count"),
                ];
                let mut values = vec![
                    string(&c.to_string()),
                    double(res as f64),
                    double(bin.count as f64),
                ];
                for ((name, op), sum) in setup.accumulate.iter().zip(&bin.sums) {
                    if let Some(v) = sum.value(*op) {
                        keys.push(Cow::Owned(name.clone()));
                        values.push(double(v));
                    }
                }
                for (name, v) in setup.median.iter().zip(bin.values.iter_mut()) {
                    if let Some(m) = median(v) {
                        keys.push(Cow::Owned(name.clone()));
                        values.push(double(m));
                    }
                }
                if let Some(category) = &setup.category {
                    let mut other = bin.uncategorized;
                    let mut counts: BTreeMap<&str, u64> = BTreeMap::new();
                    for (k, v) in &bin.categories {
                        if named.iter().any(|n| n == k) {
                            counts.insert(k.as_str(), *v);
                        } else {
                            other += v;
                        }
                    }
                    if other > 0 {
                        counts.insert("_other", other);
                    }
                    for (k, v) in &counts {
                        keys.push(Cow::Owned(format!("{}:{}", category, k)));
                        values.push(double(*v as f64));
                    }
                    // the most common value, unless two or more are
                    let most = counts.values().copied().max().unwrap_or(0);
                    let modal: Vec<&str> = counts
                        .iter()
                        .filter(|c| *c.1 == most)
                        .map(|c| *c.0)
                        .collect();
                    if let [value] = modal[..] {
                        keys.push(Cow::Borrowed("modal_category"));
                        values.push(string(value));
                    }
                    keys.extend(["modal_count", "modal_share", "modal_tie"].map(Cow::Borrowed));
                    values.extend([
                        double(most as f64),
                        double(most as f64 / bin.count as f64),
                        boolean(modal.len() > 1),
                    ]);
                }
                out.push(Hexagon {
                    layer: format!("{}_h3", layer),
                    feature: InFeature {
                        t: tippecanoe::geometry::VT_POLYGON as i8,
                        geometry: outline(c),
                        has_id: false,
                        id: 0,
                        tippecanoe_minzoom: minzoom,
                        tippecanoe_maxzoom: maxzoom,
                        full_keys: keys,
                        full_values: values,
                    },
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(args: &str) -> Setup {
        let argv: Vec<String> = std::iter::once("manby")
            .chain(args.split(' '))
            .chain(["-o", "x.pmtiles"])
            .map(String::from)
            .collect();
        Setup::new(&crate::options::parse_args(&argv).unwrap())
    }

    #[test]
    fn windows() {
        let s = setup("-z14 --h3-base-zoom=12");
        assert_eq!(
            s.windows,
            vec![
                (1, 0, 1),
                (2, 2, 3),
                (3, 4, 4),
                (4, 5, 5),
                (5, 6, 7),
                (6, 8, 8),
                (7, 9, 10),
                (8, 11, 11)
            ]
        );
        assert_eq!(s.resolutions.last(), Some(&Resolution::Eight));
        assert_eq!(s.base, 12);
        assert!(setup("-Z5 -z5 --h3").windows.is_empty());

        // given resolutions, and a zoom of overlap each way
        let s = setup("-Z2 -z8 --h3-resolutions=3:3,4:3 --h3-fade=1");
        assert_eq!(s.windows, vec![(2, 2, 3), (3, 2, 5), (4, 4, 6)]);
        assert_eq!(s.base, 5);
        assert!(!s.no_points);

        // only hexagons, to the maximum zoom
        let s = setup("-z6 --h3-no-points --h3-fade=2");
        assert_eq!(s.windows.last(), Some(&(5, 4, 6)));
        assert_eq!(s.base, 7);
        assert!(s.no_points);
    }

    #[test]
    fn resolutions_beyond_zoom_14() {
        let r: Vec<u8> = (14..=24).map(resolution).collect();
        assert_eq!(r, [10, 10, 11, 12, 13, 13, 14, 15, 15, 15, 15]);
    }

    #[test]
    fn refused() {
        for args in [
            "-z8 --h3-resolutions=6:16",
            "-z8 --h3-resolutions=7:5",
            "-z8 --h3-resolutions=x",
            "-z8 --h3-fade=-1",
            "-z8 --h3-no-points --h3-base-zoom=4",
            "-zg --h3-no-points",
            "-z8 --h3-median=pop -Epop:sum",
        ] {
            let argv: Vec<String> = std::iter::once("manby")
                .chain(args.split(' '))
                .chain(["-o", "x.pmtiles"])
                .map(String::from)
                .collect();
            let resolved =
                crate::options::parse_args(&argv).and_then(|mut o| o.resolve().map(|_| ()));
            assert!(resolved.is_err(), "{}", args);
        }
    }

    #[test]
    fn medians() {
        assert_eq!(median(&mut []), None);
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&mut [4.0, 1.0, 2.0, 10.0]), Some(3.0));
    }

    #[test]
    fn counts_and_sums() {
        let s = setup(
            "-z6 --h3-base-zoom=4 -Epop:sum -Escore2:mean --h3-median=score --h3-categories=kind",
        );
        let project = PROJECTIONS[0].project;
        let mut a = Bins::default();
        let mut b = Bins::default();
        for i in 0..4 {
            let bins = if i == 0 { &mut a } else { &mut b };
            let p = project(11.4 + i as f64 * 0.001, 48.2, 32);
            let mut keys = vec![
                Cow::Borrowed("pop"),
                Cow::Borrowed("score"),
                Cow::Borrowed("score2"),
            ];
            let score = [0.0, 1.0, 2.0, 10.0][i];
            let mut values = vec![double(10.0), double(score), double(score)];
            // the last point has no kind
            if i < 3 {
                keys.push(Cow::Borrowed("kind"));
                values.push(string(if i == 2 { "cafe" } else { "park" }));
            }
            bins.add(&s, "places", std::iter::once(p), &keys, &values);
        }
        let hexes = hexagons(&s, vec![a, b]);
        // one hexagon at each of resolutions 1 and 2, for zooms 0-1 and 2-3
        assert_eq!(hexes.len(), 2);
        let h = &hexes[0].feature;
        assert_eq!(hexes[0].layer, "places_h3");
        let get = |k: &str| {
            h.full_keys
                .iter()
                .position(|x| x == k)
                .map(|i| h.full_values[i].s.clone())
        };
        assert_eq!(get("point_count").as_deref(), Some("4"));
        assert_eq!(get("pop").as_deref(), Some("40"));
        assert_eq!(get("score2").as_deref(), Some("3.25"));
        assert_eq!(get("score").as_deref(), Some("1.5"));
        assert_eq!(get("kind:park").as_deref(), Some("2"));
        assert_eq!(get("kind:cafe").as_deref(), Some("1"));
        assert_eq!(get("kind:_other").as_deref(), Some("1"));
        assert_eq!(get("modal_category").as_deref(), Some("park"));
        assert_eq!(get("modal_count").as_deref(), Some("2"));
        assert_eq!(get("modal_share").as_deref(), Some("0.5"));
        assert_eq!(get("modal_tie").as_deref(), Some("false"));
        assert_eq!((h.tippecanoe_minzoom, h.tippecanoe_maxzoom), (0, 1));
        assert!(h.geometry.len() >= 7);
    }

    #[test]
    fn outlines_across_the_antimeridian_and_poles() {
        let r = Resolution::try_from(1u8).unwrap();
        let fiji = LatLng::new(-17.0, 179.9).unwrap().to_cell(r);
        let dv = outline(fiji);
        assert_eq!(dv.iter().filter(|d| d.op == VT_CLOSEPATH).count(), 2);
        let pole = LatLng::new(90.0, 0.0).unwrap().to_cell(r);
        let dv = outline(pole);
        assert_eq!(dv.iter().filter(|d| d.op == VT_CLOSEPATH).count(), 1);
        let ys: Vec<i64> = dv
            .iter()
            .filter(|d| d.op != VT_CLOSEPATH)
            .map(|d| d.y)
            .collect();
        assert!(
            (-1..=1).contains(ys.iter().min().unwrap()),
            "{:?}",
            ys.iter().min()
        );
    }
}
