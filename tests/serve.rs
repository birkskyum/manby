//! `manby serve` serves the same tiles from a PMTiles archive, an MBTiles
//! database and a directory of tiles made from the same features, and the
//! archive itself with range requests.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};

use manby::serve::{ServeOptions, Server};

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The body, gunzipped if it was sent gzipped.
    fn data(&self) -> Vec<u8> {
        if self.header("Content-Encoding") == Some("gzip") {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(&self.body[..])
                .read_to_end(&mut out)
                .unwrap();
            out
        } else {
            self.body.clone()
        }
    }
}

fn request(base: &str, method: &str, path: &str, extra: &str) -> Reply {
    let addr = base.trim_start_matches("http://").trim_end_matches('/');
    let mut s = TcpStream::connect(addr).unwrap();
    write!(
        s,
        "{} {} HTTP/1.1\r\nHost: {}\r\n{}Connection: close\r\n\r\n",
        method, path, addr, extra
    )
    .unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let end = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&raw[..end]).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    Reply {
        status,
        headers,
        body: raw[end + 4..].to_vec(),
    }
}

fn make(input: &Path, out: &Path, flags: &[&str]) {
    let mut argv: Vec<String> = ["tippecanoe", "-q", "-f", "-z5", "-l", "states"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    argv.extend(flags.iter().map(|s| s.to_string()));
    argv.push(input.to_string_lossy().into_owned());
    let o = manby::options::parse_args(&argv).unwrap();
    manby::run(&o).unwrap();
    assert!(out.exists());
}

#[test]
fn serves_tilesets() {
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/arrow/ne_110m_admin_1_states_provinces_lines.fgb");
    let dir = std::env::temp_dir().join(format!("manby-serve-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pmtiles = dir.join("states.pmtiles");
    let mbtiles = dir.join("states.mbtiles");
    let tiles = dir.join("tiles");
    make(&input, &pmtiles, &["-o", pmtiles.to_str().unwrap()]);
    make(&input, &mbtiles, &["-o", mbtiles.to_str().unwrap()]);
    make(&input, &tiles, &["-e", tiles.to_str().unwrap()]);

    let o = ServeOptions {
        host: "127.0.0.1".to_string(),
        port: 0,
        open: false,
        tilesets: [&pmtiles, &mbtiles, &tiles]
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
    };
    let server = Server::bind(&o).unwrap();
    let names: Vec<String> = server.tilesets().iter().map(|t| t.name.clone()).collect();
    assert_eq!(names, ["states", "states-2", "tiles"]);
    let base = server.url.clone();
    std::thread::spawn(move || server.run());

    let page = request(&base, "GET", "/", "");
    assert_eq!(page.status, 200);
    assert!(String::from_utf8_lossy(&page.body).contains("maplibre-gl"));

    let tilejson = String::from_utf8(request(&base, "GET", "/states.json", "").body).unwrap();
    assert!(
        tilejson.starts_with("{\"tilejson\":\"3.0.0\""),
        "{}",
        tilejson
    );
    assert!(
        tilejson.contains("/states/{z}/{x}/{y}.mvt?v="),
        "{}",
        tilejson
    );
    assert!(
        tilejson.contains("\"vector_layers\":[{\"id\":\"states\""),
        "{}",
        tilejson
    );
    assert!(tilejson.contains("\"maxzoom\":5"), "{}", tilejson);

    // every tile address, from each kind of tileset
    let mut found = 0;
    for z in 0..=5u32 {
        for x in 0..1u32 << z {
            for y in 0..1u32 << z {
                let replies: Vec<Reply> = names
                    .iter()
                    .map(|n| request(&base, "GET", &format!("/{}/{}/{}/{}.mvt", n, z, x, y), ""))
                    .collect();
                for r in &replies {
                    assert_eq!(r.status, replies[0].status, "{}/{}/{}", z, x, y);
                    assert_eq!(r.data(), replies[0].data(), "{}/{}/{}", z, x, y);
                    assert_eq!(r.header("Access-Control-Allow-Origin"), Some("*"));
                }
                if replies[0].status == 200 {
                    assert!(!replies[0].body.is_empty());
                    found += 1;
                } else {
                    assert_eq!(replies[0].status, 204);
                }
            }
        }
    }
    let header = std::fs::read(&pmtiles).unwrap();
    let addressed = u64::from_le_bytes(header[72..80].try_into().unwrap());
    assert_eq!(found, addressed);

    // the archive itself, whole and in part
    let whole = request(&base, "GET", "/states.pmtiles", "");
    assert_eq!(whole.status, 200);
    assert_eq!(whole.body, header);
    let part = request(&base, "GET", "/states.pmtiles", "Range: bytes=7-126\r\n");
    assert_eq!(part.status, 206);
    assert_eq!(part.body, &header[7..127]);
    assert_eq!(
        part.header("Content-Range"),
        Some(format!("bytes 7-126/{}", header.len()).as_str())
    );
    let past = request(
        &base,
        "GET",
        "/states.pmtiles",
        "Range: bytes=999999999-\r\n",
    );
    assert_eq!(past.status, 416);

    let preflight = request(&base, "OPTIONS", "/states.pmtiles", "");
    assert_eq!(preflight.status, 204);
    assert!(preflight.header("Access-Control-Allow-Methods").is_some());
    assert_eq!(request(&base, "GET", "/nothing", "").status, 404);
    assert_eq!(request(&base, "GET", "/states/9/0/0.mvt", "").status, 204);

    // Windows can't remove the tilesets while the server has them open.
    if cfg!(unix) {
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn stops() {
    let dir = std::env::temp_dir().join(format!("manby-serve-stop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/arrow/wineries.fgb");
    let out = dir.join("w.pmtiles");
    make(&input, &out, &["-o", out.to_str().unwrap()]);
    let o = ServeOptions {
        port: 0,
        host: "127.0.0.1".to_string(),
        open: false,
        tilesets: vec![out.to_string_lossy().into_owned()],
    };
    let server = Server::bind(&o).unwrap();
    let base = server.url.clone();
    let running = server.spawn();
    assert_eq!(request(&base, "GET", "/w.json", "").status, 200);
    assert_eq!(request(&base, "GET", "/w/0/0/0.mvt", "").status, 200);
    running.stop();
    // the port closes, on Windows once tiny_http's thread that accepts
    // connections has taken one more
    let addr = base.trim_start_matches("http://").trim_end_matches('/');
    let closed = (0..50).any(|_| {
        let refused = std::net::TcpStream::connect(addr).is_err();
        if !refused {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        refused
    });
    assert!(closed, "{} still open", addr);
    std::fs::remove_dir_all(&dir).unwrap();
}
