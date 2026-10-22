//! A whole run must make the same tileset with any number of threads.

use std::path::PathBuf;

fn tests_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tippecanoe-rs/tests")
}

#[test]
fn same_tiles_with_any_thread_count() {
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
