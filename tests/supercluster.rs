//! `--supercluster`: at each zoom, the tiles show the clusters and points
//! that Supercluster 8.0.1 shows, as `tests/supercluster/places.mjs` wrote
//! them down, with its properties.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::PathBuf;

use tippecanoe::mvt::{MvtTile, mvt_value_to_serial_val};

#[test]
fn clusters_as_supercluster_makes_them() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let input = root.join("crates/tippecanoe-rs/tests/ne_110m_populated_places/in.json");
    let dir = std::env::temp_dir().join(format!("manby-supercluster-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("places.pmtiles");
    let argv: Vec<String> = [
        "tippecanoe",
        "-q",
        "-f",
        "-z6",
        "--supercluster",
        "-o",
        out.to_str().unwrap(),
        input.to_str().unwrap(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    manby::run(&manby::options::parse_args(&argv).unwrap()).unwrap();

    let text = std::fs::read(root.join("tests/supercluster/places.json")).unwrap();
    let tree = tippecanoe::jsonpull::JsonPull::begin_string(&text)
        .read_tree()
        .unwrap();
    let expected: Vec<BTreeSet<String>> = tree
        .root()
        .array_iter()
        .map(|z| z.array_iter().map(|k| k.string().to_string()).collect())
        .collect();

    let t = manby::serve::tileset::Tileset::new(out.to_str().unwrap(), String::new()).unwrap();
    let opened = t.opened().unwrap();
    let mut got: Vec<BTreeSet<String>> = vec![BTreeSet::new(); expected.len()];
    for listed in opened.list().unwrap() {
        let data = opened.data(&listed).unwrap();
        let mut mvt = Vec::new();
        flate2::read::GzDecoder::new(&data[..])
            .read_to_end(&mut mvt)
            .unwrap();
        let mut tile = MvtTile::default();
        let mut compressed = false;
        assert!(tile.try_decode(&mvt, &mut compressed).unwrap());
        for layer in &tile.layers {
            for f in &layer.features {
                let get = |key: &str| {
                    f.tags.chunks(2).find_map(|kv| {
                        (layer.keys[kv[0] as usize] == key)
                            .then(|| mvt_value_to_serial_val(&layer.values[kv[1] as usize]).s)
                    })
                };
                let key = match get("cluster") {
                    Some(_) => {
                        let id = get("cluster_id").unwrap();
                        // the feature's id is the cluster's
                        assert_eq!(f.id.to_string(), id);
                        format!(
                            "cluster {} {} {} {}",
                            id,
                            get("point_count").unwrap(),
                            get("point_count_abbreviated").unwrap(),
                            get("cluster_expansion_zoom").unwrap()
                        )
                    }
                    None => format!("point {}", get("NAME").unwrap()),
                };
                got[listed.z as usize].insert(key);
            }
        }
    }
    for (z, (g, e)) in got.iter().zip(&expected).enumerate() {
        assert_eq!(g, e, "z{}", z);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn refuses_what_it_cannot_do() {
    for args in [
        "-z6 --supercluster -K10",
        "-z6 --supercluster --h3",
        "-z6 --supercluster -r2",
        "-zg --supercluster",
    ] {
        let mut argv = vec![
            "tippecanoe".to_string(),
            "-o".to_string(),
            "x.pmtiles".to_string(),
        ];
        argv.extend(args.split_whitespace().map(|s| s.to_string()));
        let mut o = manby::options::parse_args(&argv).unwrap();
        assert!(o.resolve().is_err(), "{}", args);
    }
    let argv: Vec<String> = [
        "tippecanoe",
        "-o",
        "x.pmtiles",
        "-zg",
        "-k8",
        "--supercluster",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut o = manby::options::parse_args(&argv).unwrap();
    o.resolve().unwrap();
    assert_eq!(o.supercluster_maxzoom(), 8);
    assert_eq!(o.droprate, 1.0);
}
