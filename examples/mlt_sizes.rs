//! Sizes and encoding times of a PMTiles archive's tiles as MLT, for each
//! encoder profile, with and without gzip.
//!
//!   cargo +1.98.1 run --release --features mlt --example mlt_sizes -- x.pmtiles ...

use std::collections::HashSet;
use std::time::Instant;

use manby::mlt::{MltProfile, MltSettings, encode_tile};
use rayon::prelude::*;

#[cfg(not(windows))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn main() {
    for path in std::env::args().skip(1) {
        let archive = std::fs::read(&path).unwrap();
        let mut seen = HashSet::new();
        let entries: Vec<_> = tippecanoe::pmtiles_file::pmtiles_entries_tms(&archive, 0, 32)
            .into_iter()
            .filter(|e| seen.insert(e.offset))
            .collect();
        let tiles: Vec<Vec<u8>> = entries
            .par_iter()
            .map(|e| {
                let mut raw = Vec::new();
                let data = &archive[e.offset as usize..e.offset as usize + e.length as usize];
                tippecanoe::mvt::decompress(data, &mut raw);
                raw
            })
            .collect();
        let mvt_raw: usize = tiles.iter().map(|t| t.len()).sum();
        let mvt_gz: usize = entries.iter().map(|e| e.length as usize).sum();
        println!("{}: {} distinct tiles", path, tiles.len());
        println!("  {:22} {:>12} {:>12}", "", "raw MB", "gzip MB");
        println!(
            "  {:22} {:12.2} {:12.2}",
            "MVT",
            mvt_raw as f64 / 1e6,
            mvt_gz as f64 / 1e6
        );
        for profile in [
            MltProfile::Web,
            MltProfile::Native,
            MltProfile::MinSize,
            MltProfile::Fast,
        ] {
            for gzipped in [true, false] {
                let s = MltSettings { profile, gzipped };
                let start = Instant::now();
                let sizes: Vec<(usize, usize)> = tiles
                    .par_iter()
                    .map(|t| {
                        let m = encode_tile(t, &s).unwrap();
                        let gz = if gzipped {
                            manby::compress::gzip(&m, false).len()
                        } else {
                            0
                        };
                        (m.len(), gz)
                    })
                    .collect();
                let secs = start.elapsed().as_secs_f64();
                let raw: usize = sizes.iter().map(|s| s.0).sum();
                let gz: usize = sizes.iter().map(|s| s.1).sum();
                println!(
                    "  {:22} {:12.2} {:>12} {:7.2} s",
                    format!(
                        "MLT {:?}{}",
                        profile,
                        if gzipped { " for gzip" } else { "" }
                    ),
                    raw as f64 / 1e6,
                    if gzipped {
                        format!("{:.2}", gz as f64 / 1e6)
                    } else {
                        "-".to_string()
                    },
                    secs
                );
            }
        }
    }
}
