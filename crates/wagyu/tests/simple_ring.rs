//! `union_of_simple_ring` against the sweep, on random rings: star-shaped
//! rings, which have no crossings by construction, and skylines, which have
//! horizontal edges and several points at the same height.

use manby_wagyu::{ClipType, FillType, Point, PolygonType, Wagyu, union_of_simple_ring};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> i64 {
        (self.next() % n) as i64
    }
}

fn swept(ring: &[Point]) -> Option<Vec<Vec<Vec<Point>>>> {
    let mut w = Wagyu::new();
    w.add_ring(ring, PolygonType::Subject);
    w.execute(ClipType::Union, FillType::Positive, FillType::Positive).ok()
}

fn star(rng: &mut Rng, scale: i64) -> Vec<Point> {
    let n = 3 + rng.below(40) as usize;
    let mut angles: Vec<f64> = (0..n)
        .map(|_| rng.below(1_000_000) as f64 / 1_000_000.0 * std::f64::consts::TAU)
        .collect();
    angles.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let r0 = 2 + rng.below(2000);
    let (cx, cy) = (rng.below(100_000), rng.below(100_000));
    let mut ring: Vec<Point> = angles
        .iter()
        .map(|a| {
            let r = (r0 + rng.below(r0 as u64 * 2)) as f64;
            Point::new(
                (cx + (r * a.cos()).round() as i64) * scale,
                (cy + (r * a.sin()).round() as i64) * scale,
            )
        })
        .collect();
    ring.dedup();
    ring
}

/// The outline of bars of random heights standing on a common base, upward
/// or hanging downward.
fn skyline(rng: &mut Rng, scale: i64) -> Vec<Point> {
    let bars = 1 + rng.below(12) as usize;
    let width = 1 + rng.below(20);
    let flip = if rng.below(2) == 0 { 1 } else { -1 };
    let (cx, cy) = (rng.below(100_000), rng.below(100_000));
    let mut ring = vec![Point::new(0, 0)];
    let mut x = 0;
    for _ in 0..bars {
        let h = 1 + rng.below(6) * width / 2 + rng.below(3);
        ring.push(Point::new(x, h * flip));
        x += width;
        ring.push(Point::new(x, h * flip));
    }
    ring.push(Point::new(x, 0));
    ring.dedup();
    ring.iter()
        .map(|p| Point::new((cx + p.x) * scale, (cy + p.y) * scale))
        .collect()
}

#[test]
fn the_shortcut_matches_the_sweep() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut taken = 0;
    for case in 0..60_000 {
        let scale = if case % 3 == 0 { 1 } else { 16 };
        let mut ring = if case % 2 == 0 {
            star(&mut rng, scale)
        } else {
            skyline(&mut rng, scale)
        };
        if ring.len() < 3 {
            continue;
        }
        if rng.below(2) == 0 {
            ring.reverse();
        }
        let start = rng.below(ring.len() as u64) as usize;
        ring.rotate_left(start);
        ring.push(ring[0]);

        if let Some(short) = union_of_simple_ring(&ring) {
            taken += 1;
            assert_eq!(swept(&ring), Some(short), "{ring:?}");
        }
    }
    assert!(taken > 20_000, "only {taken} rings took the shortcut");
}
