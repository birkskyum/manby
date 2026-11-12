//! A GeoPackage makes the tiles that tippecanoe makes from the FlatGeobuf
//! file GDAL writes of the same table.
//!
//! The GeoPackages in tests/gpkg/ are GDAL's of the FlatGeobuf files in
//! tests/arrow/, as sf wrote them without a spatial index. two.gpkg has two
//! tables: the wineries, and the lines of -5070.geojson in NAD83 / Conus
//! Albers.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const CASES: [&str; 4] = [
    "wineries",
    "ne_110m_admin_1_states_provinces_lines",
    "knox",
    "ne_110m_ocean",
];

const FLAGS: [&[&str]; 4] = [
    &["-z5"],
    &["-zg", "--drop-densest-as-needed"],
    &["-z6", "-r1", "--generate-ids"],
    &["-zg", "--coalesce-densest-as-needed", "-y", "name"],
];

/// A PMTiles archive's directories and tiles.
fn tiles(path: &Path) -> Vec<u8> {
    let mut f = File::open(path).unwrap();
    let mut h = [0u8; 127];
    f.read_exact(&mut h).unwrap();
    let at = |i: usize| u64::from_le_bytes(h[i..i + 8].try_into().unwrap());
    let mut out = Vec::new();
    for (o, l) in [(8, 16), (40, 48), (56, 64)] {
        let mut b = vec![0u8; at(l) as usize];
        f.seek(SeekFrom::Start(at(o))).unwrap();
        f.read_exact(&mut b).unwrap();
        out.extend_from_slice(&b);
    }
    out
}

fn run(input: &str, out: &Path, layer: &str, flags: &[&str]) -> manby::Result<Vec<u8>> {
    let mut argv: Vec<String> = ["tippecanoe", "-q", "-f", "-l", layer]
        .iter()
        .map(|s| s.to_string())
        .collect();
    argv.extend(flags.iter().map(|s| s.to_string()));
    argv.extend([
        "-o".to_string(),
        out.to_string_lossy().into_owned(),
        input.to_string(),
    ]);
    let o = manby::options::parse_args(&argv)?;
    manby::run(&o)?;
    Ok(tiles(out))
}

fn data(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("manby-gpkg-{}-{}", name, std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn same_tiles_as_flatgeobuf() {
    let dir = scratch("same");
    for case in CASES {
        for flags in FLAGS {
            let fgb = run(
                &data(&format!("arrow/{}.fgb", case)),
                &dir.join("fgb.pmtiles"),
                "layer",
                flags,
            )
            .unwrap();
            assert!(!fgb.is_empty());
            let gpkg = run(
                &data(&format!("gpkg/{}.gpkg", case)),
                &dir.join("gpkg.pmtiles"),
                "layer",
                flags,
            )
            .unwrap();
            assert!(gpkg == fgb, "{} {:?}", case, flags);
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_table_is_named_by_the_layer_and_reprojected() {
    let dir = scratch("tables");
    let two = data("gpkg/two.gpkg");
    // without a table's name, the tables are listed
    let e = manby::options::parse_args(&[
        "tippecanoe".to_string(),
        "-q".to_string(),
        "-f".to_string(),
        "-o".to_string(),
        dir.join("x.pmtiles").to_string_lossy().into_owned(),
        two.clone(),
    ])
    .and_then(|o| manby::run(&o))
    .unwrap_err();
    assert!(e.message.contains("wineries, lines"), "{}", e.message);

    for flags in FLAGS {
        let wineries = run(&two, &dir.join("a.pmtiles"), "wineries", flags).unwrap();
        let fgb = run(
            &data("arrow/wineries.fgb"),
            &dir.join("b.pmtiles"),
            "wineries",
            flags,
        )
        .unwrap();
        assert!(wineries == fgb, "wineries {:?}", flags);

        // in NAD83 / Conus Albers, as gpkg_spatial_ref_sys says
        let lines = run(&two, &dir.join("c.pmtiles"), "lines", flags).unwrap();
        let lonlat = run(
            &data("arrow/ne_110m_admin_1_states_provinces_lines.parquet"),
            &dir.join("d.pmtiles"),
            "lines",
            flags,
        )
        .unwrap();
        assert!(lines == lonlat, "lines {:?}", flags);
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
