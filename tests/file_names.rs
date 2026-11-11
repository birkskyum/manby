//! Input files whose names are not ASCII, as in tippecanoe's nonascii test,
//! given to the manby program itself, so that its arguments and the file's
//! name go through the system: on Windows as UTF-16, where the golden test
//! cannot run, since make there turns such names into question marks.

#[test]
fn layer_named_after_a_file_with_a_non_ascii_name() {
    let dir = std::env::temp_dir().join(format!("manby-file-names-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("堤防.json");
    std::fs::write(
        &input,
        r#"{"type":"Feature","properties":{},"geometry":{"type":"LineString","coordinates":[[-122.238615,37.783767],[0,0]]}}"#,
    )
    .unwrap();
    let out = dir.join("out.mbtiles");

    let status = std::process::Command::new(env!("CARGO_BIN_EXE_manby"))
        .args(["-q", "-f", "-z0", "-o"])
        .arg(&out)
        .arg(&input)
        .status()
        .unwrap();
    assert!(status.success());

    let db = rusqlite::Connection::open(&out).unwrap();
    let json: String = db
        .query_row("SELECT value FROM metadata WHERE name = 'json'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(json.contains(r#""id":"堤防""#), "layer named 堤防: {json}");
    drop(db);

    let _ = std::fs::remove_dir_all(&dir);
}
