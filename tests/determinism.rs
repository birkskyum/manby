//! A whole run must make the same tileset with any number of threads.

use std::path::PathBuf;
use std::sync::Mutex;

/// tippecanoe-rs's globals are process-wide, and a run sets them, so the
/// tests here take turns.
static GLOBALS: Mutex<()> = Mutex::new(());

fn tests_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("crates/tippecanoe-rs/tests")
}

#[test]
fn same_tiles_with_any_thread_count() {
    let _g = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    let tests = tests_dir();
    let inputs = [
        tests.join("ne_110m_populated_places/in.json"),
        tests.join("tl_2015_us_county/piscataquis.json"),
        tests.join("tl_2015_us_county/somerset.json"),
    ];
    let dir = std::env::temp_dir().join(format!("manby-determinism-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    for args in [
        "-zg --drop-densest-as-needed -P",
        "-z8 --coalesce-densest-as-needed -ai",
    ] {
        let mut first: Option<Vec<u8>> = None;
        for threads in [1, 3, 18] {
            let out = dir.join("out.pmtiles");
            let mut argv = vec![
                "tippecanoe".to_string(),
                "-q".to_string(),
                "-f".to_string(),
                "-o".to_string(),
                out.to_string_lossy().into_owned(),
            ];
            argv.extend(args.split_whitespace().map(|s| s.to_string()));
            argv.extend(inputs.iter().map(|p| p.to_string_lossy().into_owned()));
            let mut o = manby::options::parse_args(&argv).unwrap();
            o.tippecanoe_cpus = 4;
            // not on the command line, which the metadata records
            o.threads = threads;
            manby::run(&o).unwrap();
            let bytes = std::fs::read(&out).unwrap();
            match &first {
                None => first = Some(bytes),
                Some(f) => assert!(
                    *f == bytes,
                    "{}: {} threads made different tiles",
                    args,
                    threads
                ),
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A join must make the same tileset with any number of threads.
#[test]
fn join_same_tiles_with_any_thread_count() {
    let _g = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    let tests = tests_dir();
    let dir = std::env::temp_dir().join(format!("manby-join-determinism-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = |name: &str| dir.join(name).to_string_lossy().into_owned();

    // two tilesets that overlap, one deeper than the other
    for (out, args, input) in [
        ("ocean.pmtiles", "-z2", "ne_110m_ocean/in.json"),
        (
            "countries.mbtiles",
            "-z4 -d8 -y name",
            "ne_110m_admin_0_countries/in.json.gz",
        ),
    ] {
        let mut argv = vec!["tippecanoe".to_string(), "-q".to_string(), "-f".to_string()];
        argv.extend(args.split_whitespace().map(|s| s.to_string()));
        argv.extend(["-o".to_string(), path(out)]);
        argv.push(tests.join(input).to_string_lossy().into_owned());
        manby::run(&manby::options::parse_args(&argv).unwrap()).unwrap();
    }

    for args in ["", "--overzoom", "-X -l countries --no-tile-compression"] {
        let mut first: Option<Vec<u8>> = None;
        for threads in [1, 3, 18] {
            let mut argv = vec!["tile-join".to_string(), "-q".to_string(), "-f".to_string()];
            argv.extend(args.split_whitespace().map(|s| s.to_string()));
            argv.extend([
                "-o".to_string(),
                path("joined.pmtiles"),
                path("ocean.pmtiles"),
                path("countries.mbtiles"),
            ]);
            let mut o = manby::join::args::parse_args(&argv, String::new()).unwrap();
            o.threads = threads;
            manby::join(&o).unwrap();
            let bytes = std::fs::read(path("joined.pmtiles")).unwrap();
            match &first {
                None => first = Some(bytes),
                Some(f) => assert!(
                    *f == bytes,
                    "tile-join {}: {} threads made a different tileset",
                    args,
                    threads
                ),
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
