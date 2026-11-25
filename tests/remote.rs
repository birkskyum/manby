//! GeoParquet read from a URL makes the tiles that the same file makes on
//! disk, with and without `--bbox`. The files are served here, with range
//! requests, as object storage serves them, and as a presigned URL is.
#![cfg(feature = "remote")]

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

fn data() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/arrow")
}

/// What a test server takes: with `query`, only requests with that query,
/// and as S3 does for a presigned GET, no HEAD; with `redirect`, a redirect
/// of each request to that base URL.
#[derive(Clone, Default)]
struct Rules {
    query: Option<&'static str>,
    redirect: Option<String>,
}

/// A request a test server had: its method, its URL and its Referer.
type Seen = Arc<Mutex<Vec<(String, String, Option<String>)>>>;

/// Serves the files of `dir` at `http://127.0.0.1:{port}/`, with HEAD and
/// single byte ranges.
fn serve(dir: PathBuf) -> String {
    serve_with(dir, Rules::default()).0
}

/// An error as S3 answers one, with the canonical request in it.
fn refuse(request: tiny_http::Request) {
    let body = "<Error><Code>SignatureDoesNotMatch</Code><CanonicalRequest>GET /x X-Amz-Security-Token=TOKEN</CanonicalRequest></Error>";
    let _ = request.respond(tiny_http::Response::from_string(body).with_status_code(403));
}

fn serve_with(dir: PathBuf, rules: Rules) -> (String, Seen) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();
    let seen: Seen = Arc::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        for request in server.incoming_requests() {
            let referer = request
                .headers()
                .iter()
                .find(|h| h.field.equiv("Referer"))
                .map(|h| h.value.to_string());
            log.lock().unwrap().push((
                request.method().to_string(),
                request.url().to_string(),
                referer,
            ));
            let (path, query) = request
                .url()
                .split_once('?')
                .map_or((request.url(), None), |(p, q)| (p, Some(q)));
            let name = path.trim_start_matches('/').to_string();
            if let Some(q) = rules.query
                && (query != Some(q) || *request.method() == tiny_http::Method::Head)
            {
                refuse(request);
                continue;
            }
            if let Some(to) = &rules.redirect {
                let mut response = tiny_http::Response::empty(302);
                response.add_header(
                    tiny_http::Header::from_bytes(&b"Location"[..], format!("{}{}", to, name))
                        .unwrap(),
                );
                let _ = request.respond(response);
                continue;
            }
            let Ok(mut file) = File::open(dir.join(&name)) else {
                let _ = request.respond(tiny_http::Response::empty(404));
                continue;
            };
            let len = file.metadata().unwrap().len();
            let range = request
                .headers()
                .iter()
                .find(|h| h.field.equiv("Range"))
                .and_then(|h| h.value.as_str().strip_prefix("bytes=").map(String::from));
            let (status, start, end) = match range.and_then(|r| {
                let (a, b) = r.split_once('-')?;
                if a.is_empty() {
                    let n: u64 = b.parse().ok()?;
                    Some((len - n.min(len), len - 1))
                } else {
                    let a: u64 = a.parse().ok()?;
                    let b = if b.is_empty() {
                        len - 1
                    } else {
                        b.parse::<u64>().ok()?.min(len - 1)
                    };
                    Some((a, b))
                }
            }) {
                Some((a, b)) => (206, a, b),
                None => (200, 0, len - 1),
            };
            let mut body = vec![0u8; (end - start + 1) as usize];
            file.seek(SeekFrom::Start(start)).unwrap();
            file.read_exact(&mut body).unwrap();
            // with a length, not chunked, as object storage answers HEAD too
            let mut response = tiny_http::Response::from_data(body)
                .with_status_code(status)
                .with_chunked_threshold(usize::MAX);
            response.add_header(
                tiny_http::Header::from_bytes(
                    &b"Content-Range"[..],
                    format!("bytes {}-{}/{}", start, end, len),
                )
                .unwrap(),
            );
            response.add_header(
                tiny_http::Header::from_bytes(&b"Accept-Ranges"[..], &b"bytes"[..]).unwrap(),
            );
            let _ = request.respond(response);
        }
    });
    (format!("http://127.0.0.1:{}/", port), seen)
}

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

fn tile(input: &str, out: &Path, flags: &[&str]) -> manby::Result<Vec<u8>> {
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
    manby::run(&manby::options::parse_args(&argv)?)?;
    Ok(tiles(out))
}

#[test]
fn same_tiles_from_a_url() {
    let base = serve(data());
    let dir = std::env::temp_dir().join(format!("manby-remote-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("t.pmtiles");
    for case in ["ne_110m_admin_1_states_provinces_lines", "knox"] {
        let local = data().join(format!("{}.parquet", case));
        let url = format!("{}{}.parquet", base, case);
        for flags in [
            &["-z5"][..],
            &["-zg", "--drop-densest-as-needed"],
            &["-z5", "--bbox=-125,25,-80,40"],
        ] {
            let a = tile(local.to_str().unwrap(), &out, flags).unwrap();
            let b = tile(&url, &out, flags).unwrap();
            assert!(!a.is_empty());
            assert!(a == b, "{} {:?}", case, flags);
        }
    }
    let missing = tile(&format!("{}nothing.parquet", base), &out, &["-z5"]);
    assert!(missing.is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The metadata of a PMTiles archive, as JSON.
fn metadata(path: &Path) -> String {
    let mut f = File::open(path).unwrap();
    let mut h = [0u8; 127];
    f.read_exact(&mut h).unwrap();
    let at = |i: usize| u64::from_le_bytes(h[i..i + 8].try_into().unwrap());
    let mut b = vec![0u8; at(32) as usize];
    f.seek(SeekFrom::Start(at(24))).unwrap();
    f.read_exact(&mut b).unwrap();
    let mut json = String::new();
    flate2::read::GzDecoder::new(&b[..])
        .read_to_string(&mut json)
        .unwrap();
    json
}

#[test]
fn same_tiles_from_a_presigned_url() {
    let query = "X-Amz-Expires=60&X-Amz-Signature=SECRET";
    let rules = Rules {
        query: Some(query),
        ..Rules::default()
    };
    let (base, _) = serve_with(data(), rules);
    let dir = std::env::temp_dir().join(format!("manby-signed-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("t.pmtiles");
    let local = data().join("knox.parquet");
    let a = tile(local.to_str().unwrap(), &out, &["-z5"]).unwrap();
    let url = format!("{}knox.parquet?{}", base, query);
    let b = tile(&url, &out, &["-z5"]).unwrap();
    assert!(a == b);
    let json = metadata(&out);
    assert!(json.contains("knox.parquet?<redacted>"), "{}", json);
    assert!(!json.contains("SECRET"), "{}", json);

    // a wrong signature, and the error says why without the query or the
    // canonical request that S3 sends back
    let wrong = format!("{}knox.parquet?X-Amz-Signature=WRONG", base);
    let e = tile(&wrong, &out, &["-z5"]).unwrap_err().to_string();
    assert!(e.contains("knox.parquet?<redacted>"), "{}", e);
    assert!(e.contains("SignatureDoesNotMatch"), "{}", e);
    assert!(!e.contains("WRONG") && !e.contains("TOKEN"), "{}", e);

    // a presigned URL names one file
    let listing = format!("{}?X-Amz-Signature=SECRET", base);
    let e = tile(&listing, &out, &["-z5"]).unwrap_err().to_string();
    assert!(
        e.contains("names one file") && !e.contains("SECRET"),
        "{}",
        e
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_redirect_sends_no_referer() {
    let (target, seen) = serve_with(data(), Rules::default());
    let rules = Rules {
        query: Some("sig=SECRET"),
        redirect: Some(target),
    };
    let (base, _) = serve_with(data(), rules);
    let dir = std::env::temp_dir().join(format!("manby-redirect-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("t.pmtiles");
    let local = data().join("knox.parquet");
    let a = tile(local.to_str().unwrap(), &out, &["-z5"]).unwrap();
    let b = tile(&format!("{}knox.parquet?sig=SECRET", base), &out, &["-z5"]).unwrap();
    assert!(a == b);
    let seen = seen.lock().unwrap();
    assert!(!seen.is_empty());
    for (method, url, referer) in seen.iter() {
        assert!(
            referer.is_none(),
            "{} {} had Referer {:?}",
            method,
            url,
            referer
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
