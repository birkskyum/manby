//! `--supercluster`: each layer's points clustered with Supercluster 8's
//! algorithm, as MapLibre clusters a GeoJSON source. Supercluster makes a
//! hierarchy of clusters, zoom by zoom from its maximum zoom down, over the
//! points in input order. A point or cluster that it carries from one zoom
//! to the next unchanged keeps its id, so each one shows over a run of
//! zooms: here each cluster becomes a point feature with those zooms, and
//! each point shows on its own from the zoom after the one where it was last
//! clustered. A cluster's properties are Supercluster's (`cluster`,
//! `cluster_id`, `point_count`, `point_count_abbreviated`), and
//! `cluster_expansion_zoom` is what `getClusterExpansionZoom` returns, the
//! zoom after the one where the cluster was made, since every cluster has at
//! least two children.
//!
//! The index is KDBush 4's, with its 32-bit float coordinates, so that the
//! neighbors found, and the order they are found in, are Supercluster's.

/// Supercluster's options.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    /// Pixels of a tile of `extent` pixels.
    pub radius: f64,
    pub extent: f64,
    pub min_points: u64,
    pub max_zoom: i32,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            radius: 50.0,
            extent: 512.0,
            min_points: 2,
            max_zoom: 16,
        }
    }
}

/// A point or a cluster at a zoom, as Supercluster keeps them.
#[derive(Clone, Copy, Debug)]
struct Item {
    x: f64,
    y: f64,
    /// The last zoom the item was looked at in, or infinity.
    zoom: f64,
    /// The point's index in the input, or the cluster's id.
    id: i64,
    num: u64,
}

/// A cluster, in Supercluster's projection (0 to 1 on each axis).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cluster {
    pub x: f64,
    pub y: f64,
    pub id: i64,
    pub count: u64,
    /// The zooms it shows at.
    pub minzoom: i32,
    pub maxzoom: i32,
}

impl Cluster {
    /// `getClusterExpansionZoom`.
    pub fn expansion_zoom(&self) -> i32 {
        self.maxzoom + 1
    }
}

/// What clustering made of the points.
#[derive(Debug, Default)]
pub struct Clustered {
    /// For each point, the first zoom it shows at on its own.
    pub alone_from: Vec<i32>,
    pub clusters: Vec<Cluster>,
}

/// Supercluster's `lngX` and `latY`, rounded to 32 bits as it stores them.
pub fn project(lng: f64, lat: f64) -> (f64, f64) {
    let x = lng / 360.0 + 0.5;
    let sin = (lat * std::f64::consts::PI / 180.0).sin();
    let y = 0.5 - 0.25 * ((1.0 + sin) / (1.0 - sin)).ln() / std::f64::consts::PI;
    let y = y.clamp(0.0, 1.0);
    (x as f32 as f64, y as f32 as f64)
}

/// `getClusterProperties`' `point_count_abbreviated`: the count itself
/// below 1,000, else thousands as `1.2k` or `12k`.
pub fn abbreviated(count: u64) -> Result<u64, String> {
    if count >= 10000 {
        Err(format!("{}k", (count as f64 / 1000.0).round()))
    } else if count >= 1000 {
        Err(format!("{}k", (count as f64 / 100.0).round() / 10.0))
    } else {
        Ok(count)
    }
}

/// Clusters `points`, given in Supercluster's projection, at every zoom from
/// `settings.max_zoom` down to 0.
pub fn cluster(points: &[(f64, f64)], settings: &Settings) -> Clustered {
    let mut out = Clustered {
        alone_from: vec![0; points.len()],
        clusters: Vec::new(),
    };
    let mut items: Vec<Item> = points
        .iter()
        .enumerate()
        .map(|(i, &(x, y))| Item {
            x,
            y,
            zoom: f64::INFINITY,
            id: i as i64,
            num: 1,
        })
        .collect();
    // where each cluster is in `out.clusters`
    let mut made: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    for z in (0..=settings.max_zoom).rev() {
        let tree = KdBush::new(&items, 64);
        let next = cluster_zoom(
            &mut items,
            &tree,
            z,
            settings,
            points.len() as u64,
            |item| {
                if item.num == 1 {
                    out.alone_from[item.id as usize] = z + 1;
                } else if let Some(&c) = made.get(&item.id) {
                    out.clusters[c].minzoom = z + 1;
                }
            },
        );
        for item in &next {
            if item.num > 1 && !made.contains_key(&item.id) {
                made.insert(item.id, out.clusters.len());
                out.clusters.push(Cluster {
                    x: item.x,
                    y: item.y,
                    id: item.id,
                    count: item.num,
                    minzoom: 0,
                    maxzoom: z,
                });
            }
        }
        items = next;
    }
    out
}

/// Supercluster's `_cluster`: the items of the next zoom down from those of
/// `zoom + 1`. `merged` hears of each item that goes into a cluster.
fn cluster_zoom(
    items: &mut [Item],
    tree: &KdBush,
    zoom: i32,
    s: &Settings,
    points: u64,
    mut merged: impl FnMut(&Item),
) -> Vec<Item> {
    let r = s.radius / (s.extent * 2f64.powi(zoom));
    let z = zoom as f64;
    let mut next = Vec::new();
    let mut neighbors: Vec<u32> = Vec::new();
    for i in 0..items.len() {
        if items[i].zoom <= z {
            continue;
        }
        items[i].zoom = z;
        let (x, y) = (items[i].x, items[i].y);
        neighbors.clear();
        tree.within(x, y, r, &mut neighbors);

        let origin = items[i].num;
        let mut num = origin;
        for &k in &neighbors {
            let k = &items[k as usize];
            if k.zoom > z {
                num += k.num;
            }
        }

        if num > origin && num >= s.min_points {
            let mut wx = x * origin as f64;
            let mut wy = y * origin as f64;
            // JavaScript shifts in 32 bits
            let id = ((i as i32).wrapping_shl(5)) as i64 + (zoom + 1) as i64 + points as i64;
            for &k in &neighbors {
                let item = &mut items[k as usize];
                if item.zoom <= z {
                    continue;
                }
                item.zoom = z;
                wx += item.x * item.num as f64;
                wy += item.y * item.num as f64;
                merged(item);
            }
            merged(&items[i]);
            next.push(Item {
                x: wx / num as f64,
                y: wy / num as f64,
                zoom: f64::INFINITY,
                id,
                num,
            });
        } else {
            next.push(items[i]);
            if num > 1 {
                for &k in &neighbors {
                    let item = &mut items[k as usize];
                    if item.zoom <= z {
                        continue;
                    }
                    item.zoom = z;
                    next.push(*item);
                }
            }
        }
    }
    next
}

/// KDBush 4 over 32-bit float coordinates.
struct KdBush {
    ids: Vec<u32>,
    coords: Vec<f32>,
    node_size: i64,
}

impl KdBush {
    fn new(items: &[Item], node_size: i64) -> KdBush {
        let mut t = KdBush {
            ids: (0..items.len() as u32).collect(),
            coords: items
                .iter()
                .flat_map(|i| [i.x as f32, i.y as f32])
                .collect(),
            node_size,
        };
        if !items.is_empty() {
            t.sort(0, items.len() as i64 - 1, 0);
        }
        t
    }

    fn c(&self, i: i64, axis: i64) -> f32 {
        self.coords[(2 * i + axis) as usize]
    }

    fn swap_item(&mut self, i: i64, j: i64) {
        self.ids.swap(i as usize, j as usize);
        self.coords.swap(2 * i as usize, 2 * j as usize);
        self.coords.swap(2 * i as usize + 1, 2 * j as usize + 1);
    }

    fn sort(&mut self, left: i64, right: i64, axis: i64) {
        if right - left <= self.node_size {
            return;
        }
        let m = (left + right) >> 1;
        self.select(m, left, right, axis);
        self.sort(left, m - 1, 1 - axis);
        self.sort(m + 1, right, 1 - axis);
    }

    /// Floyd-Rivest selection, as KDBush has it.
    fn select(&mut self, k: i64, mut left: i64, mut right: i64, axis: i64) {
        while right > left {
            if right - left > 600 {
                let n = (right - left + 1) as f64;
                let m = (k - left + 1) as f64;
                let z = n.ln();
                let s = 0.5 * (2.0 * z / 3.0).exp();
                let sd =
                    0.5 * (z * s * (n - s) / n).sqrt() * if m - n / 2.0 < 0.0 { -1.0 } else { 1.0 };
                let new_left = left.max((k as f64 - m * s / n + sd).floor() as i64);
                let new_right = right.min((k as f64 + (n - m) * s / n + sd).floor() as i64);
                self.select(k, new_left, new_right, axis);
            }

            let t = self.c(k, axis);
            let mut i = left;
            let mut j = right;

            self.swap_item(left, k);
            if self.c(right, axis) > t {
                self.swap_item(left, right);
            }

            while i < j {
                self.swap_item(i, j);
                i += 1;
                j -= 1;
                while self.c(i, axis) < t {
                    i += 1;
                }
                while self.c(j, axis) > t {
                    j -= 1;
                }
            }

            if self.c(left, axis) == t {
                self.swap_item(left, j);
            } else {
                j += 1;
                self.swap_item(j, right);
            }

            if j <= k {
                left = j + 1;
            }
            if k <= j {
                right = j - 1;
            }
        }
    }

    /// The items within `r` of `qx`, `qy`, in KDBush's order.
    fn within(&self, qx: f64, qy: f64, r: f64, out: &mut Vec<u32>) {
        if self.ids.is_empty() {
            return;
        }
        let r2 = r * r;
        let mut stack: Vec<(i64, i64, i64)> = vec![(0, self.ids.len() as i64 - 1, 0)];
        let d2 = |x: f32, y: f32| {
            let dx = x as f64 - qx;
            let dy = y as f64 - qy;
            dx * dx + dy * dy
        };
        while let Some((left, right, axis)) = stack.pop() {
            if right - left <= self.node_size {
                for i in left..=right {
                    if d2(self.c(i, 0), self.c(i, 1)) <= r2 {
                        out.push(self.ids[i as usize]);
                    }
                }
                continue;
            }
            let m = (left + right) >> 1;
            let (x, y) = (self.c(m, 0), self.c(m, 1));
            if d2(x, y) <= r2 {
                out.push(self.ids[m as usize]);
            }
            let v = if axis == 0 { x as f64 } else { y as f64 };
            let q = if axis == 0 { qx } else { qy };
            if q - r <= v {
                stack.push((left, m - 1, 1 - axis));
            }
            if q + r >= v {
                stack.push((m + 1, right, 1 - axis));
            }
        }
    }
}

/// A cluster as a point feature, with Supercluster's properties and its
/// id as the feature's id, at the zooms it shows at.
pub fn feature(c: &Cluster) -> super::serialize::InFeature<'static> {
    use std::borrow::Cow;
    use tippecanoe::geometry::{Draw, VT_MOVETO, VT_POINT};
    use tippecanoe::milo::dtoa_milo;
    use tippecanoe::mvt::{MVT_BOOL, MVT_DOUBLE, MVT_STRING};
    use tippecanoe::serial::SerialVal;

    let number = |n: f64| SerialVal::new(MVT_DOUBLE, &dtoa_milo(n));
    let world = 4294967296.0;
    super::serialize::InFeature {
        t: VT_POINT as i8,
        geometry: vec![Draw::new(
            VT_MOVETO,
            (c.x * world).round() as i64,
            (c.y * world).round() as i64,
        )],
        has_id: c.id >= 0,
        id: c.id.max(0) as u64,
        tippecanoe_minzoom: c.minzoom,
        tippecanoe_maxzoom: c.maxzoom,
        full_keys: [
            "cluster",
            "cluster_id",
            "point_count",
            "point_count_abbreviated",
            "cluster_expansion_zoom",
        ]
        .into_iter()
        .map(Cow::Borrowed)
        .collect(),
        full_values: vec![
            SerialVal::new(MVT_BOOL, "true"),
            number(c.id as f64),
            number(c.count as f64),
            match abbreviated(c.count) {
                Ok(n) => number(n as f64),
                Err(s) => SerialVal::new(MVT_STRING, &s),
            },
            number(c.expansion_zoom() as f64),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abbreviations() {
        assert_eq!(abbreviated(999), Ok(999));
        assert_eq!(abbreviated(1000), Err("1k".to_string()));
        assert_eq!(abbreviated(1250), Err("1.3k".to_string()));
        assert_eq!(abbreviated(9999), Err("10k".to_string()));
        assert_eq!(abbreviated(12345), Err("12k".to_string()));
    }

    /// The same clusters as Supercluster 8.0.1 makes of the points in
    /// `tests/supercluster/points.json`, which `make.mjs` there wrote.
    #[test]
    fn same_as_supercluster() {
        use tippecanoe::jsonpull::{JsonPull, JsonType};
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/supercluster/points.json"
        );
        let text = std::fs::read(path).unwrap();
        let tree = JsonPull::begin_string(&text).read_tree().unwrap();
        let root = tree.root();
        let num = |v: tippecanoe::jsonpull::JsonRef<'_>| {
            assert_eq!(v.type_(), JsonType::Number);
            v.number()
        };
        let options = root.hash_get("options").unwrap();
        let settings = Settings {
            radius: num(options.hash_get("radius").unwrap()),
            extent: num(options.hash_get("extent").unwrap()),
            min_points: num(options.hash_get("minPoints").unwrap()) as u64,
            max_zoom: num(options.hash_get("maxZoom").unwrap()) as i32,
        };
        let points: Vec<(f64, f64)> = root
            .hash_get("points")
            .unwrap()
            .array_iter()
            .map(|p| project(num(p.array_get(0)), num(p.array_get(1))))
            .collect();
        let got = cluster(&points, &settings);

        let alone: Vec<i32> = root
            .hash_get("alone_from")
            .unwrap()
            .array_iter()
            .map(|v| num(v) as i32)
            .collect();
        assert_eq!(got.alone_from, alone);
        let mut want: Vec<Cluster> = root
            .hash_get("clusters")
            .unwrap()
            .array_iter()
            .map(|c| Cluster {
                id: num(c.hash_get("id").unwrap()) as i64,
                count: num(c.hash_get("count").unwrap()) as u64,
                x: num(c.hash_get("x").unwrap()),
                y: num(c.hash_get("y").unwrap()),
                minzoom: num(c.hash_get("minzoom").unwrap()) as i32,
                maxzoom: num(c.hash_get("maxzoom").unwrap()) as i32,
            })
            .collect();
        let mut got = got.clusters;
        want.sort_by_key(|c| c.id);
        got.sort_by_key(|c| c.id);
        assert_eq!(got.len(), want.len());
        for (g, w) in got.iter().zip(&want) {
            assert_eq!(g, w);
        }
        // and the expansion zooms Supercluster gives
        let zooms = root.hash_get("expansion").unwrap();
        for (id, z) in zooms.entries_iter() {
            let id: i64 = id.string().parse().unwrap();
            let c = got.iter().find(|c| c.id == id).unwrap();
            assert_eq!(c.expansion_zoom(), num(z) as i32);
        }
    }
}
