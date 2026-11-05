//! Checks `union_of_simple_ring` against the sweep on a recording of cleanings
//! (the format of `wagyu_replay`), and times both.
use manby_wagyu::{ClipType, FillType, Point, PolygonType, Wagyu, union_of_simple_ring};
use std::time::Instant;

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
}
fn i64_at(b: &[u8], i: usize) -> i64 {
    i64::from_le_bytes(b[i..i + 8].try_into().unwrap())
}

fn main() {
    let b = std::fs::read(std::env::args().nth(1).expect("a recording")).unwrap();
    let (mut single, mut taken, mut different) = (0u64, 0u64, 0u64);
    let (mut t_short, mut t_sweep_taken, mut t_sweep_all) = (0f64, 0f64, 0f64);
    let mut i = 0;
    while i + 4 <= b.len() {
        let nrings = u32_at(&b, i) as usize;
        i += 4;
        let mut rings = Vec::new();
        for _ in 0..nrings {
            let n = u32_at(&b, i + 1) as usize;
            i += 5;
            rings.push((0..n).map(|k| Point::new(i64_at(&b, i + 16 * k), i64_at(&b, i + 16 * k + 8))).collect::<Vec<_>>());
            i += 16 * n;
        }
        if nrings != 1 {
            continue;
        }
        single += 1;
        let t = Instant::now();
        let short = union_of_simple_ring(&rings[0]);
        t_short += t.elapsed().as_secs_f64();
        let t = Instant::now();
        let mut w = Wagyu::new();
        w.add_ring(&rings[0], PolygonType::Subject);
        let swept = w.execute(ClipType::Union, FillType::Positive, FillType::Positive);
        let ts = t.elapsed().as_secs_f64();
        t_sweep_all += ts;
        if let Some(s) = short {
            taken += 1;
            t_sweep_taken += ts;
            if swept.as_ref().ok() != Some(&s) {
                different += 1;
                if different <= 3 {
                    eprintln!("different: {:?}", rings[0]);
                }
            }
        }
    }
    println!(
        "{single} single rings, {taken} taken, {different} different; shortcut test {:.2} s, sweep on the rings taken {:.2} s, on all single rings {:.2} s",
        t_short, t_sweep_taken, t_sweep_all
    );
    if different > 0 {
        std::process::exit(1);
    }
}
