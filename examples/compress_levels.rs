//! Compares zlib at tippecanoe's settings with every libdeflate level on the
//! uncompressed tiles of PMTiles archives: total size and time on one thread.
//!
//!   cargo run --release --example compress_levels -- x.pmtiles ...

use std::collections::HashSet;
use std::time::Instant;

use libdeflater::{CompressionLvl, Compressor};

fn main() {
    for path in std::env::args().skip(1) {
        let archive = std::fs::read(&path).unwrap();
        let mut seen = HashSet::new();
        let tiles: Vec<Vec<u8>> = tippecanoe::pmtiles_file::pmtiles_entries_tms(&archive, 0, 32)
            .into_iter()
            .filter(|e| seen.insert(e.offset))
            .map(|e| {
                let mut raw = Vec::new();
                let data = &archive[e.offset as usize..e.offset as usize + e.length as usize];
                tippecanoe::mvt::decompress(data, &mut raw);
                raw
            })
            .collect();
        let raw: usize = tiles.iter().map(|t| t.len()).sum();
        println!(
            "{}: {} tiles, {:.1} MB",
            path,
            tiles.len(),
            raw as f64 / 1e6
        );

        let mut gzip = manby::compress::Gzip::new();
        let mut out = Vec::new();
        let start = Instant::now();
        let zlib: usize = tiles
            .iter()
            .map(|t| {
                gzip.compress_into(t, false, &mut out);
                out.len()
            })
            .sum();
        println!(
            "  zlib 6        {:9.2} MB {:7.2} s",
            zlib as f64 / 1e6,
            start.elapsed().as_secs_f64()
        );

        for level in 1..=12 {
            let mut c = Compressor::new(CompressionLvl::new(level).unwrap());
            let start = Instant::now();
            let size: usize = tiles
                .iter()
                .map(|t| {
                    out.resize(c.gzip_compress_bound(t.len()), 0);
                    c.gzip_compress(t, &mut out).unwrap()
                })
                .sum();
            println!(
                "  libdeflate {:2} {:9.2} MB {:7.2} s {:+6.2}%",
                level,
                size as f64 / 1e6,
                start.elapsed().as_secs_f64(),
                100.0 * (size as f64 - zlib as f64) / zlib as f64
            );
        }
    }
}
