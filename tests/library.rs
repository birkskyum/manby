//! manby as a library: the options as fields, as the crate's first page
//! shows them, from outside the crate.

use std::path::PathBuf;

use manby::options::Output;
use manby::{JoinOptions, Options};

#[test]
fn tiles_and_joins_from_fields() {
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("crates/tippecanoe-rs/tests/ne_110m_populated_places/in.json");
    let dir = std::env::temp_dir().join(format!("manby-library-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = |name: &str| dir.join(name).to_string_lossy().into_owned();

    let mut options = Options::default();
    options.inputs = vec![input.to_string_lossy().into_owned()];
    options.output = Output::Pmtiles(path("places.pmtiles"));
    options.maxzoom = 4;
    options.quiet = true;
    let summary = manby::run(&options).unwrap();
    assert_eq!((summary.minzoom, summary.maxzoom), (0, 4));
    assert_eq!(summary.features, 243);
    assert!(summary.tiles > 0 && !summary.incomplete);

    // the same options as a command line make the same tiles
    let args = [
        "manby",
        "-q",
        "-z4",
        "-o",
        &path("places-args.pmtiles"),
        &options.inputs[0],
    ];
    let parsed = manby::options::parse_args(&args.map(String::from)).unwrap();
    assert_eq!(manby::run(&parsed).unwrap().tiles, summary.tiles);

    let mut join = JoinOptions::default();
    join.inputs = vec![path("places.pmtiles")];
    join.output = Output::Mbtiles(path("joined.mbtiles"));
    join.quiet = true;
    let joined = manby::join(&join).unwrap();
    assert_eq!(joined.tiles, summary.tiles);
    assert_eq!((joined.minzoom, joined.maxzoom), (0, 4));

    let report = manby::stats::report(&path("joined.mbtiles"), false, 0).unwrap();
    let tiles: u64 = report.zooms.iter().map(|z| z.tiles).sum();
    assert_eq!(tiles, summary.tiles);

    std::fs::remove_dir_all(&dir).ok();
}
