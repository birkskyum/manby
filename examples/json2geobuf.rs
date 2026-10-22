//! A port of `json2geobuf` from the geobuf npm package (version 3.0.2, with
//! pbf 3.3.0), which the `geobuf-test` target of the Makefile uses to turn
//! the GeoJSON test fixtures into Geobuf. It reads GeoJSON on stdin (or from
//! the file named by the first argument if stdin is a terminal) and writes
//! the same Geobuf bytes as the JavaScript tool.

#[path = "json2geobuf/encode.rs"]
mod encode;
#[path = "json2geobuf/js.rs"]
mod js;
#[path = "json2geobuf/pbf.rs"]
mod pbf;

use std::io::{IsTerminal, Read, Write};

fn run() {
    // var input = process.stdin.isTTY ? fs.createReadStream(process.argv[2]) : process.stdin;
    let mut buf = Vec::new();
    let read = if std::io::stdin().is_terminal() {
        match std::env::args_os().nth(1) {
            Some(path) => std::fs::File::open(path).and_then(|mut f| f.read_to_end(&mut buf)),
            None => js::throw("TypeError: The \"path\" argument must be of type string"),
        }
    } else {
        std::io::stdin().lock().read_to_end(&mut buf)
    };
    if let Err(err) = read {
        js::throw(&format!("Error: {err}"));
    }

    // buf.toString() decodes UTF-8, replacing invalid sequences with U+FFFD
    let geojson = js::json_parse(&String::from_utf8_lossy(&buf));
    drop(buf);

    let buffer = encode::encode(&geojson, pbf::Pbf::new());

    let mut stdout = std::io::stdout().lock();
    if let Err(err) = stdout.write_all(&buffer).and_then(|()| stdout.flush()) {
        js::throw(&format!("Error: {err}"));
    }
}

fn main() {
    // Deeply nested JSON recurses as deeply in the parser and in JSON.stringify.
    let worker = std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(run)
        .expect("spawn");
    if worker.join().is_err() {
        std::process::exit(1);
    }
}
