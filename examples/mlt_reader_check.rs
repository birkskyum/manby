//! Checks that manby's MLT encoder, which reads MVT bytes itself, makes the
//! same bytes as mlt-core's own MVT reader, on every tile of PMTiles
//! archives of MVT tiles and for every encoder profile.
//!
//!   cargo +1.98.1 run --release --features mlt --example mlt_reader_check -- x.pmtiles ...

use std::collections::HashSet;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

use manby::mlt::{MltProfile, MltSettings, encode_tile, encode_tile_with_mlt_core};
use rayon::prelude::*;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> ExitCode {
    let mut failed = false;
    for path in std::env::args().skip(1) {
        let archive = std::fs::read(&path).unwrap();
        let mut seen = HashSet::new();
        let entries: Vec<_> = tippecanoe::pmtiles_file::pmtiles_entries_tms(&archive, 0, 32)
            .into_iter()
            .filter(|e| seen.insert(e.offset))
            .collect();
        for profile in [
            MltProfile::Web,
            MltProfile::Native,
            MltProfile::MinSize,
            MltProfile::Fast,
        ] {
            for gzipped in [true, false] {
                let s = MltSettings { profile, gzipped };
                let (same, different, refused) =
                    (AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0));
                entries.par_iter().for_each(|e| {
                    let mut mvt = Vec::new();
                    let data = &archive[e.offset as usize..e.offset as usize + e.length as usize];
                    tippecanoe::mvt::decompress(data, &mut mvt);
                    let ours = encode_tile(&mvt, &s).unwrap();
                    match encode_tile_with_mlt_core(&mvt, &s) {
                        Ok(theirs) if theirs == ours => &same,
                        Ok(_) => {
                            eprintln!("{}: {}/{}/{} differs", path, e.z, e.x, e.y);
                            &different
                        }
                        Err(_) => &refused,
                    }
                    .fetch_add(1, Ordering::Relaxed);
                });
                let different = different.load(Ordering::Relaxed);
                failed |= different > 0;
                println!(
                    "{} {:?}{}: {} tiles the same, {} different, {} too large for mlt-core's reader",
                    path,
                    profile,
                    if gzipped { " for gzip" } else { "" },
                    same.load(Ordering::Relaxed),
                    different,
                    refused.load(Ordering::Relaxed)
                );
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
