//! A tileset made in shards and merged is the tileset of the whole run, byte
//! for byte.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// tippecanoe-rs's globals are process-wide, and a run sets them, so the
/// tests here take turns.
static GLOBALS: Mutex<()> = Mutex::new(());

fn tests_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("crates/tippecanoe-rs/tests")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("manby-shard-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn options(out: &Path, args: &str, inputs: &[&str]) -> manby::Options {
    let mut argv = vec![
        "tippecanoe".to_string(),
        "-q".to_string(),
        "-f".to_string(),
        "-o".to_string(),
        out.to_string_lossy().into_owned(),
    ];
    argv.extend(args.split_whitespace().map(|s| s.to_string()));
    argv.extend(
        inputs
            .iter()
            .map(|p| tests_dir().join(p).to_string_lossy().into_owned()),
    );
    let mut o = manby::options::parse_args(&argv).unwrap();
    o.tippecanoe_cpus = 4;
    o
}

/// Makes the tileset whole and in `n` shards with `shard_args`, merges the
/// shards, and checks that the two are the same. Returns the merge's summary.
fn compare(
    name: &str,
    n: u32,
    args: &str,
    shard_args: &str,
    inputs: &[&str],
) -> manby::MergeSummary {
    let dir = scratch(name);
    let out = dir.join("out.pmtiles");
    let whole = options(&out, args, inputs);
    manby::run(&whole).unwrap();
    let expected = std::fs::read(&out).unwrap();
    std::fs::remove_file(&out).unwrap();

    let mut parts = Vec::new();
    for i in 0..n {
        let part = dir.join(format!("part-{}.pmtiles", i));
        let args = format!("{} --shard {}/{} {}", args, i, n, shard_args);
        let mut o = options(&part, &args, inputs);
        // the whole run's, which differs only in the output's name
        o.commandline = whole.commandline.clone();
        manby::run(&o).unwrap();
        parts.push(part.to_string_lossy().into_owned());
    }
    // in any order
    parts.reverse();
    let mut argv = vec![
        "manby merge".to_string(),
        "-q".to_string(),
        "-o".to_string(),
        out.to_string_lossy().into_owned(),
    ];
    argv.extend(parts);
    let summary = manby::merge(&manby::shard::parse_args(&argv).unwrap()).unwrap();
    let merged = std::fs::read(&out).unwrap();
    assert!(
        merged == expected,
        "{}: {} shards with {} {} made a different tileset (in {})",
        name,
        n,
        args,
        shard_args,
        dir.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
    summary
}

#[test]
fn points() {
    let _g = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    let places = ["ne_110m_populated_places/in.json"];
    let s = compare("points", 3, "-zg --drop-densest-as-needed", "", &places);
    assert!(s.differ.is_empty());
    compare("points-z0", 2, "-z6", "--shard-zoom 0", &places);
    compare(
        "clusters",
        3,
        "-z10 -r1 --cluster-distance=20 --accumulate-attribute=POP_MAX:sum",
        "",
        &places,
    );
    compare(
        "extend",
        2,
        "-zg --extend-zooms-if-still-dropping",
        "",
        &places,
    );
}

#[test]
fn polygons() {
    let _g = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    let counties = [
        "tl_2015_us_county/piscataquis.json",
        "tl_2015_us_county/somerset.json",
    ];
    compare("counties", 3, "-z11", "", &counties);
    compare("counties-z8", 4, "-z11", "--shard-zoom 8", &counties);
    compare(
        "counties-dropping",
        3,
        "-z12 --drop-densest-as-needed -M 30000",
        "",
        &counties,
    );
    let countries = ["ne_110m_admin_0_countries/in.json.gz"];
    compare("countries-minzoom", 3, "-z8 -Z3", "", &countries);
    compare(
        "countries-coalescing",
        4,
        "-z7 --coalesce-densest-as-needed -M 20000",
        "",
        &countries,
    );
}

#[test]
fn lines() {
    let _g = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    compare(
        "lines",
        3,
        "-z6",
        "",
        &["ne_110m_admin_1_states_provinces_lines/in.json"],
    );
}

/// A zoom that one shard makes again with stricter thresholds is made with
/// them only for its tiles, which the merge says; with
/// `--as-needed-per-tile`, the shards make the same tiles as a whole run.
#[test]
fn thresholds_raised_below_the_pivot() {
    let _g = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    let places = ["ne_110m_populated_places/in.json"];
    let args = "-z5 --drop-densest-as-needed -M 3000 --as-needed-per-tile";
    let s = compare("per-tile", 4, args, "--shard-zoom 1", &places);
    assert!(s.differ.is_empty());

    let dir = scratch("again");
    let mut parts = Vec::new();
    for i in 0..4 {
        let part = dir.join(format!("part-{}.pmtiles", i));
        let args = format!(
            "-z5 --drop-densest-as-needed -M 3000 --shard {}/4 --shard-zoom 1",
            i
        );
        manby::run(&options(&part, &args, &places)).unwrap();
        parts.push(part.to_string_lossy().into_owned());
    }
    let mut argv = vec![
        "manby merge".to_string(),
        "-q".to_string(),
        "-o".to_string(),
    ];
    argv.push(dir.join("out.pmtiles").to_string_lossy().into_owned());
    argv.extend(parts);
    let s = manby::merge(&manby::shard::parse_args(&argv).unwrap()).unwrap();
    assert!(!s.differ.is_empty());
    assert!(s.differ.iter().all(|&z| z >= 1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn merge_checks_the_shards() {
    let _g = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("checks");
    let places = ["ne_110m_populated_places/in.json"];
    let part = |name: &str, args: &str| {
        let p = dir.join(name);
        manby::run(&options(&p, args, &places)).unwrap();
        p.to_string_lossy().into_owned()
    };
    let a0 = part("a0.pmtiles", "-z5 --shard 0/2");
    let a1 = part("a1.pmtiles", "-z5 --shard 1/2");
    let b1 = part("b1.pmtiles", "-z6 --shard 1/2");
    let whole = part("whole.pmtiles", "-z5");
    let out = dir.join("out.pmtiles").to_string_lossy().into_owned();
    let merge = |inputs: &[&str]| {
        let mut argv = vec![
            "manby merge".to_string(),
            "-q".to_string(),
            "-f".to_string(),
            "-o".to_string(),
            out.clone(),
        ];
        argv.extend(inputs.iter().map(|s| s.to_string()));
        manby::merge(&manby::shard::parse_args(&argv).unwrap()).map_err(|e| e.message)
    };
    assert!(merge(&[&a0, &a1]).is_ok());
    let e = merge(&[&a0]).unwrap_err();
    assert!(e.contains("missing shard 1/2"), "{}", e);
    let e = merge(&[&a0, &a0]).unwrap_err();
    assert!(e.contains("are both shard 0/2"), "{}", e);
    let e = merge(&[&a0, &b1]).unwrap_err();
    assert!(e.contains("another run"), "{}", e);
    let e = merge(&[&a0, &whole]).unwrap_err();
    assert!(e.contains("not a shard"), "{}", e);
    let _ = std::fs::remove_dir_all(&dir);
}
