//! A shapefile makes the tiles that tippecanoe makes from the FlatGeobuf
//! file GDAL writes of it.
//!
//! The shapefiles in tests/shapefile/ are GDAL's, as sf wrote them: of the
//! FlatGeobuf files in tests/arrow/, the oceans being 121 rings of one
//! polygon and a second polygon, and of a table of each dBASE type with
//! nulls, in UTF-8 and in ISO-8859-1. The GeoPackages are GDAL's of the
//! shapefiles, and manby's GeoPackage reader makes the tiles of GDAL's
//! FlatGeobuf (tests/gpkg.rs). knox-26916 is knox in NAD83 / UTM zone 16N,
//! with the ESRI `.prj` GDAL writes.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const CASES: [&str; 5] = ["ocean", "knox", "wineries", "types", "latin1"];

const FLAGS: [&[&str]; 4] = [
    &["-z5"],
    &["-zg", "--drop-densest-as-needed"],
    &["-z6", "-r1", "--generate-ids"],
    &["-z8", "--coalesce-densest-as-needed"],
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

fn run(input: &str, out: &Path, flags: &[&str]) -> manby::Result<Vec<u8>> {
    let mut argv: Vec<String> = ["tippecanoe", "-q", "-f", "-l", "layer"]
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
        .join("tests/shapefile")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("manby-shp-{}-{}", name, std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn same_tiles_as_gdal() {
    let dir = scratch("same");
    for case in CASES {
        for flags in FLAGS {
            let gpkg = run(
                &data(&format!("{}.gpkg", case)),
                &dir.join("gpkg.pmtiles"),
                flags,
            )
            .unwrap();
            assert!(!gpkg.is_empty());
            let shp = run(
                &data(&format!("{}.shp", case)),
                &dir.join("shp.pmtiles"),
                flags,
            )
            .unwrap();
            assert!(shp == gpkg, "{} {:?}", case, flags);
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn reprojected_by_the_prj_or_by_s() {
    let dir = scratch("crs");
    for flags in FLAGS {
        let lonlat = run(&data("knox.shp"), &dir.join("a.pmtiles"), flags).unwrap();
        let utm = run(&data("knox-26916.shp"), &dir.join("b.pmtiles"), flags).unwrap();
        assert!(utm == lonlat, "{:?}", flags);
        let mut with_s = flags.to_vec();
        with_s.extend(["-s", "EPSG:26916"]);
        let s = run(&data("knox-26916.shp"), &dir.join("c.pmtiles"), &with_s).unwrap();
        assert!(s == lonlat, "-s {:?}", flags);
    }

    // a CRS manby doesn't know by its .prj
    let odd = dir.join("odd.shp");
    for ext in ["shp", "shx", "dbf"] {
        std::fs::copy(
            data(&format!("knox-26916.{}", ext)),
            odd.with_extension(ext),
        )
        .unwrap();
    }
    std::fs::write(
        odd.with_extension("prj"),
        r#"PROJCS["Somewhere_Grid",GEOGCS["GCS_Somewhere",DATUM["D_Somewhere",SPHEROID["X",6378137.0,298.257222101]]],PROJECTION["Transverse_Mercator"]]"#,
    )
    .unwrap();
    let e = run(&odd.to_string_lossy(), &dir.join("d.pmtiles"), &["-z5"]).unwrap_err();
    assert!(e.message.contains("Somewhere_Grid"), "{}", e.message);
    std::fs::remove_dir_all(&dir).unwrap();
}
