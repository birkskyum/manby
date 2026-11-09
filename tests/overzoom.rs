//! `manby overzoom` makes the tiles of tippecanoe-rs's overzoom tests, and
//! filters with `like`, `ilike` and `intersects` from a filter file.

use std::path::{Path, PathBuf};
use std::process::Command;

use tippecanoe::mvt::{MVT_STRING, MvtTile};
use tippecanoe::projection::lonlat2tile;

fn pbf_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("crates/tippecanoe-rs/tests/pbf")
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("manby-overzoom-{}-{}", name, std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn manby(args: &[&str]) -> std::process::Output {
    let out = Command::new(env!("CARGO_BIN_EXE_manby"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "manby {:?}: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// `manby decode` of one tile, as the tests' expected files have it.
fn decode(tile: &Path, zxy: &str) -> Vec<u8> {
    let mut args = vec!["decode", tile.to_str().unwrap()];
    args.extend(zxy.split('/'));
    manby(&args).stdout
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

/// tippecanoe-rs's `overzoom-test` recipes, as in its Makefile, that need no
/// tiles made first.
#[test]
fn tippecanoe_overzoom_tests() {
    let pbf = pbf_dir();
    let dir = temp_dir("recipes");
    let out = dir.join("out.pbf");
    let src = |name: &str| pbf.join(name).to_string_lossy().into_owned();
    let scalerank_filter = src("scalerank-0-filter.json");

    // (arguments before the output, the inputs and tiles, the tile to decode, the expected decoding)
    let cases: &[(&[&str], Vec<String>, &str, &str)] = &[
        (
            &[],
            vec![
                src("11-327-791.pbf"),
                "11/327/791".into(),
                "13/1310/3166".into(),
            ],
            "13/1310/3166",
            "13-1310-3166.pbf.json",
        ),
        (
            &["-t", "13/1310/3166"],
            vec![src("11-327-791.pbf"), "11/327/791".into()],
            "13/1310/3166",
            "13-1310-3166.pbf.json",
        ),
        (
            &["--no-tile-compression", "-t", "13/1310/3166"],
            vec![
                src("11-327-791.pbf"),
                "11/327/791".into(),
                src("0-0-0.pbf"),
                "0/0/0".into(),
            ],
            "13/1310/3166",
            "13-1310-3166-ne.pbf.json",
        ),
        (
            &["-d8", "-b30", "-y", "NAME", "-y", "name", "-y", "scalerank"],
            vec![
                src("11-327-791.pbf"),
                "11/327/791".into(),
                "13/1310/3166".into(),
            ],
            "13/1310/3166",
            "13-1310-3166-8-30.pbf.json",
        ),
        (
            &["-y", "NAME", "-m"],
            vec![src("0-0-0-pop.pbf"), "0/0/0".into(), "0/0/0".into()],
            "0/0/0",
            "0-0-0-pop-filtered.pbf.json",
        ),
        (
            &["-y", "NAME", "-m", "--accumulate-attribute", "NAME:comma"],
            vec![src("0-0-0-pop.pbf"), "0/0/0".into(), "0/0/0".into()],
            "0/0/0",
            "0-0-0-pop-accum.pbf.json",
        ),
        (
            &["-y", "NAME", r#"-j{"*":["==","SCALERANK",0]}"#],
            vec![src("0-0-0-pop.pbf"), "0/0/0".into(), "0/0/0".into()],
            "0/0/0",
            "0-0-0-pop-expr.pbf.json",
        ),
        (
            &["-y", "NAME", "-J", &scalerank_filter],
            vec![src("0-0-0-pop.pbf"), "0/0/0".into(), "0/0/0".into()],
            "0/0/0",
            "0-0-0-pop-expr.pbf.json",
        ),
        (
            &[
                "-y",
                "NAME",
                "-y",
                "SCALERANK",
                r#"-j{"*":["==","SCALERANK",0]}"#,
                "-m",
            ],
            vec![src("0-0-0-pop.pbf"), "0/0/0".into(), "0/0/0".into()],
            "0/0/0",
            "0-0-0-filter-mult.pbf.json",
        ),
        (
            &[
                "-y",
                "NAME",
                "-y",
                "SCALERANK",
                "-y",
                "LATITUDE",
                r#"-j{"*":[">","LATITUDE",40]}"#,
                "-m",
                "--preserve-input-order",
            ],
            vec![src("0-0-0-pop.pbf"), "0/0/0".into(), "0/0/0".into()],
            "0/0/0",
            "0-0-0-filter-mult-order.pbf.json",
        ),
        (
            &["--line-simplification=5", "--tiny-polygon-size=50"],
            vec![src("countries-0-0-0.pbf"), "0/0/0".into(), "0/0/0".into()],
            "0/0/0",
            "countries-0-0-0.pbf.out.json",
        ),
    ];
    for (options, inputs, zxy, expected) in cases {
        let mut args = vec!["overzoom"];
        args.extend_from_slice(options);
        args.extend(["-o", s(&out)]);
        args.extend(inputs.iter().map(|a| a.as_str()));
        manby(&args);
        let want = std::fs::read(pbf.join(expected)).unwrap();
        assert!(decode(&out, zxy) == want, "{:?}", args);
    }

    // a tile kept as it is, in the order of its features
    let src_tile = pbf.join("11-327-791.pbf");
    manby(&[
        "overzoom",
        "-b20",
        "--preserve-input-order",
        "-o",
        s(&out),
        s(&src_tile),
        "11/327/791",
        "11/327/791",
    ]);
    assert!(decode(&out, "11/327/791") == decode(&src_tile, "11/327/791"));

    // no features in the child tile: an empty file
    manby(&[
        "overzoom",
        "-o",
        s(&out),
        s(&src_tile),
        "11/327/791",
        "14/2616/6331",
    ]);
    assert_eq!(std::fs::read(&out).unwrap().len(), 0);

    // two tiles with features of the same ids, with and without deduplication
    let mut sources = Vec::new();
    for name in ["1.json", "2.json"] {
        let tiles = dir.join(format!("{name}.dir"));
        manby(&[
            "-q",
            "-z0",
            "-f",
            "-e",
            s(&tiles),
            "-l",
            "layer",
            s(&pbf.join(name)),
        ]);
        sources.push(tiles.join("0/0/0.pbf").to_string_lossy().into_owned());
    }
    for (dedup, expected) in [
        (None, "merged-nodedup.pbf.json"),
        (Some("--deduplicate-by-id"), "merged-dedup.pbf.json"),
    ] {
        let mut args = vec!["overzoom", "-b0"];
        args.extend(dedup);
        args.extend(["-o", s(&out), "-t", "1/1/0"]);
        args.extend([sources[0].as_str(), "0/0/0", sources[1].as_str(), "0/0/0"]);
        manby(&args);
        let want = std::fs::read(pbf.join(expected)).unwrap();
        assert!(decode(&out, "1/1/0") == want, "{:?}", args);
    }

    std::fs::remove_dir_all(&dir).unwrap();
}

/// The populated places of `0-0-0-pop.pbf`, each with its name and its point
/// in the tile's 4096 units, in the tile's order.
fn places(tile: &[u8]) -> Vec<(String, i64, i64)> {
    let mut t = MvtTile::default();
    assert!(t.try_decode(tile, &mut false).unwrap());
    let mut out = Vec::new();
    for layer in &t.layers {
        for f in &layer.features {
            let mut name = String::new();
            for kv in f.tags.chunks(2) {
                let v = &layer.values[kv[1] as usize];
                if layer.keys[kv[0] as usize] == "NAME" && v.type_ == MVT_STRING {
                    name = v.get_str().to_string();
                }
            }
            let scale = 4096.0 / layer.extent as f64;
            let p = &f.geometry[0];
            out.push((
                name,
                (p.x as f64 * scale) as i64,
                (p.y as f64 * scale) as i64,
            ));
        }
    }
    out
}

#[test]
fn like_ilike_and_intersects_from_a_filter_file() {
    let pbf = pbf_dir();
    let dir = temp_dir("filter");
    let src = pbf.join("0-0-0-pop.pbf");
    let out = dir.join("out.mvt.gz");
    let filter_file = dir.join("filter.json");

    let overzoom = |subcommand: &str, filter: Option<&str>| {
        let mut args = vec![subcommand, "-o", s(&out), "-b", "5"];
        if let Some(filter) = filter {
            std::fs::write(&filter_file, filter).unwrap();
            args.extend(["--feature-filter-file", s(&filter_file)]);
        }
        args.extend([s(&src), "0/0/0", "0/0/0"]);
        manby(&args);
        let tile = std::fs::read(&out).unwrap();
        assert_eq!(&tile[..2], &[0x1f, 0x8b], "gzip whatever the name");
        places(&tile)
    };

    let all = overzoom("overzoom", None);
    assert!(all.len() > 200);

    // Europe, in longitude and latitude, as the tile's units
    let (w, s_, e, n) = (-10.0, 35.0, 30.0, 60.0);
    let (x0, y0) = lonlat2tile(w, n, 32);
    let (x1, y1) = lonlat2tile(e, s_, 32);
    let in_europe = move |p: &(String, i64, i64)| {
        let (x, y) = (p.1 << 20, p.2 << 20);
        x0 <= x && x <= x1 && y0 <= y && y <= y1
    };
    let europe = format!(
        r#"{{"type": "Polygon", "coordinates": [[[{w}, {s_}], [{e}, {s_}], [{e}, {n}], [{w}, {n}], [{w}, {s_}]]]}}"#
    );
    // with a second part far from any place, around the South Pole
    let europe_and_more = format!(
        r#"{{"type": "MultiPolygon", "coordinates": [[[[-10, -89], [10, -89], [10, -85], [-10, -85], [-10, -89]]], [[[{w}, {s_}], [{e}, {s_}], [{e}, {n}], [{w}, {n}], [{w}, {s_}]]]]}}"#
    );

    type Pick = Box<dyn Fn(&(String, i64, i64)) -> bool>;
    let cases: Vec<(String, Pick)> = vec![
        (
            r#"{"*": ["like", "NAME", "San%"]}"#.to_string(),
            Box::new(|p| p.0.starts_with("San")),
        ),
        (
            r#"{"*": ["ilike", "NAME", "%BER%"]}"#.to_string(),
            Box::new(|p| p.0.to_lowercase().contains("ber")),
        ),
        (
            r#"{"*": ["!like", "NAME", "_a%"]}"#.to_string(),
            Box::new(|p| p.0.chars().nth(1) != Some('a')),
        ),
        (
            format!(r#"{{"*": ["intersects", {europe}]}}"#),
            Box::new(in_europe),
        ),
        (
            format!(r#"{{"*": ["intersects", {europe_and_more}]}}"#),
            Box::new(in_europe),
        ),
        (
            format!(
                r#"{{"*": ["any", ["none", ["intersects", {europe}]], ["all", ["ilike", "NAME", "b%"], ["has", "SCALERANK"]]]}}"#
            ),
            Box::new(move |p| !in_europe(p) || p.0.to_lowercase().starts_with('b')),
        ),
    ];
    for (filter, pick) in &cases {
        let want: Vec<_> = all.iter().filter(|p| pick(p)).cloned().collect();
        assert!(!want.is_empty() && want.len() < all.len(), "{filter}");
        for subcommand in ["overzoom", "tippecanoe-overzoom"] {
            assert_eq!(overzoom(subcommand, Some(filter)), want, "{filter}");
        }
    }

    std::fs::remove_dir_all(&dir).unwrap();
}
