//! `--no-simplification-of-shared-nodes` with lines that collapse to nothing.

/// A street segment from OpenStreetMap whose two points are less than a
/// millimetre apart: both land on the same point of the 32-bit world grid, so
/// removing the repeated point leaves no geometry at all.
const COLLAPSING: &str = r#"{"type":"FeatureCollection","features":[
{"type":"Feature","properties":{"n":1},"geometry":{"type":"LineString","coordinates":[[11.46082965934974,48.25778546568767],[11.4608297,48.2577855]]}},
{"type":"Feature","properties":{"n":2},"geometry":{"type":"LineString","coordinates":[[11.45,48.25],[11.47,48.26],[11.48,48.25]]}}
]}"#;

/// The collapsed line is kept, as tippecanoe keeps it: its layer counts both
/// features and both values, where tippecanoe 2.49 writes the same tilestats.
#[test]
fn line_that_collapses_to_nothing() {
    let dir = std::env::temp_dir().join(format!("manby-shared-nodes-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("in.geojson");
    std::fs::write(&input, COLLAPSING).unwrap();
    let out = dir.join("out.mbtiles");

    let argv: Vec<String> = [
        "tippecanoe",
        "-q",
        "-f",
        "-z14",
        "--no-simplification-of-shared-nodes",
        "-o",
        &out.to_string_lossy(),
        &input.to_string_lossy(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let o = manby::options::parse_args(&argv).unwrap();
    manby::run(&o).unwrap();

    let db = rusqlite::Connection::open(&out).unwrap();
    let json: String = db
        .query_row("SELECT value FROM metadata WHERE name = 'json'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let tiles: i64 = db
        .query_row("SELECT count(*) FROM tiles", [], |r| r.get(0))
        .unwrap();
    assert!(
        json.contains(r#""layer":"in","count":2,"#),
        "both features counted: {json}"
    );
    assert!(
        json.contains(r#""values":[1,2]"#),
        "both values kept: {json}"
    );
    assert_eq!(tiles, 21, "the tiles tippecanoe 2.49 makes");
    let _ = std::fs::remove_dir_all(&dir);
}
