//! Times tile compression: tippecanoe-rs's `mvt::compress`, manby's default
//! gzip (the same bytes, with zlib's state reused) and fast mode (libdeflate),
//! on the uncompressed tiles of a PMTiles archive.
//!
//!   cargo run --release --example compress_bench -- archive.pmtiles

use std::time::Instant;

use rayon::prelude::*;

type Compress<'a> = &'a (dyn Fn(&Vec<u8>) -> Vec<u8> + Sync);

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: compress_bench archive.pmtiles");
    let archive = std::fs::read(&path).unwrap();
    let tiles: Vec<Vec<u8>> = tippecanoe::pmtiles_file::pmtiles_entries_tms(&archive, 0, 32)
        .par_iter()
        .map(|e| {
            let data = &archive[e.offset as usize..e.offset as usize + e.length as usize];
            let mut raw = Vec::new();
            tippecanoe::mvt::decompress(data, &mut raw);
            raw
        })
        .collect();
    let bytes: usize = tiles.iter().map(|t| t.len()).sum();
    println!(
        "{} tiles, {:.1} MB uncompressed",
        tiles.len(),
        bytes as f64 / 1e6
    );

    let tippecanoe = |t: &Vec<u8>| {
        let mut out = Vec::new();
        tippecanoe::mvt::compress(t, &mut out, true);
        out
    };
    let default = |t: &Vec<u8>| manby::compress::gzip(t, false);
    let fast = |t: &Vec<u8>| manby::compress::gzip(t, true);
    let methods: [(&str, Compress); 3] = [
        ("tippecanoe-rs", &tippecanoe),
        ("manby", &default),
        ("manby fast", &fast),
    ];

    for (name, f) in methods {
        let start = Instant::now();
        let size: usize = tiles.iter().map(|t| f(t).len()).sum();
        let one = start.elapsed().as_secs_f64();
        let start = Instant::now();
        let _: usize = tiles.par_iter().map(|t| f(t).len()).sum();
        let all = start.elapsed().as_secs_f64();
        println!(
            "{:14} 1 thread {:7.3} s   {} threads {:6.3} s   {:.1} MB",
            name,
            one,
            rayon::current_num_threads(),
            all,
            size as f64 / 1e6
        );
    }

    let same = tiles.par_iter().all(|t| tippecanoe(t) == default(t));
    println!(
        "default gzip {} tippecanoe-rs's",
        if same { "same as" } else { "DIFFERENT FROM" }
    );
}
