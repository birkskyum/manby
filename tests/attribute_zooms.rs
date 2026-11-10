//! `--use-attribute-for-minzoom` and `--use-attribute-for-maxzoom`: an
//! attribute gives a feature its zooms, as the "tippecanoe" object of a
//! GeoJSON feature does, and so the tiles are the ones tippecanoe makes of
//! that GeoJSON.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::{ArrayRef, Int32Array, RecordBatch};
use arrow_ipc::writer::StreamWriter;
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

const ZOOMS: [&str; 2] = [
    "--use-attribute-for-minzoom=from",
    "--use-attribute-for-maxzoom=to",
];

/// A PMTiles archive's directories and tiles.
fn tiles(path: &Path) -> Vec<u8> {
    let mut f = File::open(path).unwrap();
    let mut h = [0u8; 127];
    f.read_exact(&mut h).unwrap();
    let at = |i: usize| u64::from_le_bytes(h[i..i + 8].try_into().unwrap());
    let mut out = Vec::new();
    // the root directory, the leaf directories and the tiles
    for (o, l) in [(8, 16), (40, 48), (56, 64)] {
        let mut b = vec![0u8; at(l) as usize];
        f.seek(SeekFrom::Start(at(o))).unwrap();
        f.read_exact(&mut b).unwrap();
        out.extend_from_slice(&b);
    }
    out
}

fn run(input: &Path, out: &Path, flags: &[&str]) -> manby::Result<manby::Summary> {
    let mut argv: Vec<String> = ["tippecanoe", "-q", "-f", "-l", "p"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    argv.extend(flags.iter().map(|s| s.to_string()));
    argv.extend([
        "-o".to_string(),
        out.to_string_lossy().into_owned(),
        input.to_string_lossy().into_owned(),
    ]);
    manby::run(&manby::options::parse_args(&argv)?)
}

fn tile(input: &Path, out: &Path, flags: &[&str]) -> Vec<u8> {
    run(input, out, flags).unwrap();
    tiles(out)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("manby-zooms-{}-{}", name, std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The zooms of point `i`: some have both, some one, and some neither.
fn zooms(i: usize) -> (Option<usize>, Option<usize>) {
    match i % 7 {
        k @ 0..=1 => (Some(k + 1), None),
        2 => (Some(3), Some(4)),
        3 => (None, Some(5)),
        _ => (None, None),
    }
}

#[test]
fn same_tiles_as_the_geojson_extension() {
    let dir = scratch("geojson");
    // the zooms as attributes, as GeoJSON and as CSV
    let mut attributes = File::create(dir.join("attributes.geojsonl")).unwrap();
    let mut csv = File::create(dir.join("attributes.csv")).unwrap();
    // the zooms as tippecanoe reads them from GeoJSON
    let mut extension = File::create(dir.join("extension.geojsonl")).unwrap();
    writeln!(csv, "lon,lat,name,from,to").unwrap();
    for i in 0..3000 {
        let lon = -10.0 + (i * 7919 % 3000) as f64 / 75.0;
        let lat = 35.0 + (i * 104729 % 3000) as f64 / 120.0;
        let (from, to) = zooms(i);
        let mut properties = format!(r#""name":"p{i}""#);
        let mut tippecanoe = Vec::new();
        if let Some(z) = from {
            properties += &format!(r#","from":{z}"#);
            tippecanoe.push(format!(r#""minzoom":{z}"#));
        } else if i % 7 == 4 {
            properties += r#","from":null"#;
        }
        if let Some(z) = to {
            properties += &format!(r#","to":{z}"#);
            tippecanoe.push(format!(r#""maxzoom":{z}"#));
        }
        let geometry = format!(r#""geometry":{{"type":"Point","coordinates":[{lon},{lat}]}}"#);
        writeln!(
            attributes,
            r#"{{"type":"Feature","properties":{{{properties}}},{geometry}}}"#
        )
        .unwrap();
        writeln!(
            extension,
            r#"{{"type":"Feature","tippecanoe":{{{}}},"properties":{{"name":"p{i}"}},{geometry}}}"#,
            tippecanoe.join(",")
        )
        .unwrap();
        let cell = |z: Option<usize>| z.map(|z| z.to_string()).unwrap_or_default();
        writeln!(csv, "{lon},{lat},p{i},{},{}", cell(from), cell(to)).unwrap();
    }
    drop((attributes, csv, extension));

    for flags in [
        &["-z6"][..],
        &["-z6", "-r1"],
        &["-zg", "--drop-densest-as-needed"],
        &["-z7", "--cluster-distance=20"],
    ] {
        let expected = tile(
            &dir.join("extension.geojsonl"),
            &dir.join("e.pmtiles"),
            flags,
        );
        assert!(!expected.is_empty());
        let with = [flags, &ZOOMS[..]].concat();
        let geojson = tile(
            &dir.join("attributes.geojsonl"),
            &dir.join("a.pmtiles"),
            &with,
        );
        assert!(geojson == expected, "{:?}: GeoJSON", flags);
    }

    // tippecanoe leaves CSV out of what -zg measures, so with a maximum zoom
    for flags in [&["-z6"][..], &["-z6", "-r1"]] {
        let expected = tile(
            &dir.join("extension.geojsonl"),
            &dir.join("e.pmtiles"),
            flags,
        );
        let with = [flags, &ZOOMS[..]].concat();
        let csv = tile(&dir.join("attributes.csv"), &dir.join("c.pmtiles"), &with);
        assert!(csv == expected, "{:?}: CSV", flags);
    }

    // The attributes are the zooms now, and in neither the tiles nor the
    // metadata, whose JSON the decoding quotes once more.
    let decoded = std::process::Command::new(env!("CARGO_BIN_EXE_manby"))
        .arg("decode")
        .arg(dir.join("a.pmtiles"))
        .output()
        .unwrap();
    let decoded = String::from_utf8_lossy(&decoded.stdout);
    assert!(decoded.contains(r#"\"name\":\"String\""#));
    assert!(decoded.contains(r#""name": "p0""#));
    for name in ["from", "to"] {
        assert!(!decoded.contains(&format!(r#""{name}""#)), "{name}");
        assert!(!decoded.contains(&format!(r#""{name}\""#)), "{name}");
    }

    std::fs::remove_dir_all(&dir).unwrap();
}

/// The wineries with a column of zooms, null in every fifth row, as an Arrow
/// stream. Returns how many rows show at each zoom up to 4.
fn wineries_with_zooms(out: &Path) -> [u64; 5] {
    let parquet = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/arrow/wineries.parquet");
    let reader = ParquetRecordBatchReaderBuilder::try_new(File::open(parquet).unwrap())
        .unwrap()
        .with_batch_size(37)
        .build()
        .unwrap();
    let old = arrow_array::RecordBatchReader::schema(&reader);
    let mut fields: Vec<Field> = old.fields().iter().map(|f| f.as_ref().clone()).collect();
    fields.push(Field::new("from", DataType::Int32, true));
    let schema = Arc::new(Schema::new_with_metadata(fields, old.metadata().clone()));
    let mut w = StreamWriter::try_new(File::create(out).unwrap(), &schema).unwrap();
    let mut shown = [0u64; 5];
    let mut row = 0;
    for b in reader {
        let b = b.unwrap();
        let from: Int32Array = (0..b.num_rows())
            .map(|i| {
                let i = row + i;
                let z = (i % 5 != 0).then_some((i % 4) as i32 + 1);
                for (zoom, n) in shown.iter_mut().enumerate() {
                    *n += z.is_none_or(|z| z <= zoom as i32) as u64;
                }
                z
            })
            .collect();
        row += b.num_rows();
        let mut columns = b.columns().to_vec();
        columns.push(Arc::new(from) as ArrayRef);
        w.write(&RecordBatch::try_new(schema.clone(), columns).unwrap())
            .unwrap();
    }
    w.finish().unwrap();
    shown
}

#[test]
fn a_column_of_a_table() {
    let dir = scratch("arrow");
    let stream = dir.join("wineries.arrows");
    let shown = wineries_with_zooms(&stream);
    assert!(shown[0] > 0 && shown[0] < shown[4], "{shown:?}");

    // no features dropped, and no buffer, so each point is in one tile
    let out = dir.join("w.pmtiles");
    run(
        &stream,
        &out,
        &["-z4", "-r1", "-b0", "--use-attribute-for-minzoom=from"],
    )
    .unwrap();
    let report = manby::stats::report(out.to_str().unwrap(), true, 0).unwrap();
    for z in &report.zooms {
        assert_eq!(z.layers["p"].features, shown[z.z as usize], "z{}", z.z);
    }
    assert_eq!(report.zooms.len(), 5);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_value_that_is_no_zoom() {
    let dir = scratch("bad");
    for (value, flags, fails) in [
        ("3.5", &["--use-attribute-for-minzoom=from"][..], true),
        ("40", &["--use-attribute-for-maxzoom=from"], true),
        ("\"3\"", &["--use-attribute-for-minzoom=from"], true),
        // -T makes the text a number first
        (
            "\"3\"",
            &["--use-attribute-for-minzoom=from", "-T", "from:int"],
            false,
        ),
        ("\"\"", &["--use-attribute-for-minzoom=from"], false),
        ("3", &["--use-attribute-for-minzoom=from"], false),
    ] {
        let input = dir.join("in.geojson");
        std::fs::write(
            &input,
            format!(
                r#"{{"type":"Feature","properties":{{"from":{value}}},"geometry":{{"type":"Point","coordinates":[1,2]}}}}"#
            ),
        )
        .unwrap();
        let r = run(&input, &dir.join("o.pmtiles"), &[&["-z5"], flags].concat());
        match r {
            Err(e) => {
                assert!(fails, "{value} {flags:?}: {e}");
                let text = e.to_string();
                assert!(
                    text.contains("Expected integer zoom level in attribute \"from\""),
                    "{text}"
                );
            }
            Ok(_) => assert!(!fails, "{value} {flags:?}"),
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
