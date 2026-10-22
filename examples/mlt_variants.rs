//! Writes the tiles of an MVT PMTiles archive as raw MLT tile directories,
//! one per encoder variant, for checking which MLT decoders read which
//! encodings.
//!
//!   cargo +1.98.1 run --release --features mlt --example mlt_variants -- in.pmtiles out/

use std::collections::HashSet;

use manby::mlt::{EncoderConfig, MltProfile, MltSettings, encode_tile_with, encoder_config};
use rayon::prelude::*;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let archive = std::fs::read(&args[1]).unwrap();
    let mut seen = HashSet::new();
    let entries: Vec<_> = tippecanoe::pmtiles_file::pmtiles_entries_tms(&archive, 0, 32)
        .into_iter()
        .filter(|e| seen.insert((e.z, e.x, e.y)))
        .collect();
    let web = |gzipped| {
        encoder_config(&MltSettings {
            profile: MltProfile::Web,
            gzipped,
        })
    };
    let variants: Vec<(&str, EncoderConfig, bool)> = vec![
        ("web-gzip", web(true), true),
        ("web-gzip-dict", web(true).with_shared_dict(true), true),
        ("web-gzip-wide", web(true), false),
        ("web-raw", web(false), true),
        ("web-raw-fsst", web(false).with_fastpfor(false), true),
        ("web-raw-fastpfor", web(false).with_fsst(false), true),
        (
            "native",
            encoder_config(&MltSettings {
                profile: MltProfile::Native,
                gzipped: true,
            }),
            false,
        ),
    ];
    for (name, config, narrow) in variants {
        let dir = format!("{}/{}", args[2], name);
        entries.par_iter().for_each(|e| {
            let mut mvt = Vec::new();
            let data = &archive[e.offset as usize..e.offset as usize + e.length as usize];
            tippecanoe::mvt::decompress(data, &mut mvt);
            let mlt = encode_tile_with(&mvt, config, narrow).unwrap();
            let d = format!("{}/{}/{}", dir, e.z, e.x);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(format!("{}/{}.mlt", d, e.y), mlt).unwrap();
        });
        println!("{}: {} tiles", dir, entries.len());
    }
}
