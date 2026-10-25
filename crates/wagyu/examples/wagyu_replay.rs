//! Replays recorded polygon cleanings through tippecanoe-rs's wagyu and this one,
//! checks that they make the same polygons, and adds up the time each takes.
//!
//! `cargo run --release -p manby-wagyu --example wagyu_replay -- calls.bin [every] [ours]`
//!
//! The file holds one record per cleaning: the number of rings as a u32, then for
//! each ring its type (0 subject, 1 clip) as a u8, its number of points as a u32
//! and the points as pairs of i64, all little-endian. Every cleaning is a union
//! with positive fill, as tippecanoe's are. `every` replays only every nth record,
//! and `ours` runs only this wagyu, for profiling it. `REPLAY_THREADS` sets the
//! number of threads, all of the cores by default.

use std::time::{Duration, Instant};

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

/// The byte ranges of the records, stopping at a record cut short.
fn records(b: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    'next: while let Some(nrings) = u32_at(b, i) {
        let start = i;
        i += 4;
        for _ in 0..nrings {
            let Some(n) = u32_at(b, i + 1) else {
                break 'next;
            };
            i += 5 + n as usize * 16;
            if i > b.len() {
                break 'next;
            }
        }
        out.push((start, i));
    }
    out
}

type Rings = Vec<(u8, Vec<(i64, i64)>)>;

fn parse(b: &[u8]) -> Rings {
    let nrings = u32_at(b, 0).unwrap();
    let mut i = 4;
    let mut rings = Vec::with_capacity(nrings as usize);
    for _ in 0..nrings {
        let t = b[i];
        let n = u32_at(b, i + 1).unwrap() as usize;
        i += 5;
        let mut pts = Vec::with_capacity(n);
        for _ in 0..n {
            let x = i64::from_le_bytes(b[i..i + 8].try_into().unwrap());
            let y = i64::from_le_bytes(b[i + 8..i + 16].try_into().unwrap());
            pts.push((x, y));
            i += 16;
        }
        rings.push((t, pts));
    }
    rings
}

type Out = Result<Vec<Vec<Vec<(i64, i64)>>>, String>;

fn run_port(rings: &Rings) -> Out {
    use port_wagyu::{ClipType, FillType, Point, PolygonType, Wagyu};
    let mut w = Wagyu::new();
    for (t, pts) in rings {
        let lr: Vec<Point> = pts.iter().map(|&(x, y)| Point::new(x, y)).collect();
        w.add_ring(
            &lr,
            if *t == 0 {
                PolygonType::Subject
            } else {
                PolygonType::Clip
            },
        );
    }
    w.execute(ClipType::Union, FillType::Positive, FillType::Positive)
        .map(|mp| {
            mp.iter()
                .map(|p| {
                    p.iter()
                        .map(|r| r.iter().map(|q| (q.x, q.y)).collect())
                        .collect()
                })
                .collect()
        })
        .map_err(|e| e.to_string())
}

fn run_ours(rings: &Rings) -> Out {
    use wagyu::{ClipType, FillType, Point, PolygonType, Wagyu};
    let mut w = Wagyu::new();
    for (t, pts) in rings {
        let lr: Vec<Point> = pts.iter().map(|&(x, y)| Point::new(x, y)).collect();
        w.add_ring(
            &lr,
            if *t == 0 {
                PolygonType::Subject
            } else {
                PolygonType::Clip
            },
        );
    }
    w.execute(ClipType::Union, FillType::Positive, FillType::Positive)
        .map(|mp| {
            mp.iter()
                .map(|p| {
                    p.iter()
                        .map(|r| r.iter().map(|q| (q.x, q.y)).collect())
                        .collect()
                })
                .collect()
        })
        .map_err(|e| e.to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bytes = std::fs::read(&args[1]).expect("read the recording");
    let every: usize = args.get(2).map_or(1, |s| s.parse().unwrap());
    let only_ours = args.get(3).is_some_and(|s| s == "ours");
    let recs: Vec<(usize, usize)> = records(&bytes).into_iter().step_by(every).collect();
    let threads = std::env::var("REPLAY_THREADS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(8, |n| n.get()));
    let results: Vec<(Duration, Duration, Vec<usize>)> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let (bytes, recs) = (&bytes, &recs);
                s.spawn(move || {
                    let (mut port, mut ours, mut bad) =
                        (Duration::ZERO, Duration::ZERO, Vec::new());
                    for k in (t..recs.len()).step_by(threads) {
                        let (a, b) = recs[k];
                        let rings = parse(&bytes[a..b]);
                        let t0 = Instant::now();
                        let p = if only_ours {
                            Ok(Vec::new())
                        } else {
                            run_port(&rings)
                        };
                        let t1 = Instant::now();
                        let o = run_ours(&rings);
                        let t2 = Instant::now();
                        port += t1 - t0;
                        ours += t2 - t1;
                        if !only_ours && p != o {
                            bad.push(k);
                        }
                    }
                    (port, ours, bad)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let port: Duration = results.iter().map(|r| r.0).sum();
    let ours: Duration = results.iter().map(|r| r.1).sum();
    let mut bad: Vec<usize> = results.into_iter().flat_map(|r| r.2).collect();
    bad.sort();
    println!(
        "{} cleanings: tippecanoe-rs's wagyu {:.2} s, manby's {:.2} s ({:.2}x), {} different{}",
        recs.len(),
        port.as_secs_f64(),
        ours.as_secs_f64(),
        port.as_secs_f64() / ours.as_secs_f64(),
        bad.len(),
        if bad.is_empty() {
            String::new()
        } else {
            format!(", first at record {}", bad[0] * every)
        }
    );
    if !bad.is_empty() {
        std::process::exit(1);
    }
}
