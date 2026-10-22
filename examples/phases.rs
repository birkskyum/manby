//! Times manby's phases on a tippecanoe command line, without tiling:
//!
//!     cargo run --release --example phases -- -zg -P bench/data/points4m.geojsonl

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    let mut argv: Vec<String> = std::env::args().collect();
    argv.splice(
        1..1,
        ["-o".to_string(), "/tmp/manby-phases.pmtiles".to_string()],
    );
    let mut o = match manby::options::parse_args(&argv) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{}", e.message);
            std::process::exit(e.exit_code);
        }
    };
    o.quiet = true;
    o.resolve().unwrap();
    manby::options::set_tippecanoe_globals(&o, o.cluster_maxzoom);

    let t = std::time::Instant::now();
    let mut store = manby::ingest::read_inputs(&o).unwrap();
    eprintln!(
        "read {} features in {:.3}s",
        store.features.len(),
        t.elapsed().as_secs_f64()
    );

    let t = std::time::Instant::now();
    let plan = manby::order::order(&mut store, &o).unwrap();
    eprintln!("ordered in {:.3}s: {:?}", t.elapsed().as_secs_f64(), plan);
}
