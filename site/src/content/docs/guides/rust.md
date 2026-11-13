---
title: From Rust
description: Tiling and joining from a Rust program with the manby crate, which returns errors instead of exiting.
---

The `manby` crate is the library that the `manby` command is a thin layer over. It reads, tiles and writes in the calling process, and returns an error where tippecanoe would exit. [docs.rs](https://docs.rs/manby) has every type.

```sh
cargo add manby
```

It needs Rust 1.90 or later. Two cargo features add what the releases have built in.

- **`remote`** reads GeoParquet from `s3://`, `gs://`, `az://` and `https://` URLs.
- **`mlt`** writes [MapLibre Tiles](../mlt/), and needs Rust 1.98.

## Tiling

The options are tippecanoe's, so its command line is the shortest way to them. `parse_args` takes the arguments with the program's name first, and `run` makes the tileset.

```rust
let args = ["manby", "-zg", "--drop-densest-as-needed", "-o", "places.pmtiles", "places.geojson"];
let options = manby::options::parse_args(&args.map(String::from))?;
let summary = manby::run(&options)?;
println!("{} tiles, to zoom {}", summary.tiles, summary.maxzoom);
```

The options are also fields of `Options`, set on its defaults, which are tippecanoe's. Each field's description names the option it is.

```rust
use manby::options::Output;

let mut options = manby::Options::default();
options.inputs = vec!["places.geojson".to_string()];
options.output = Output::Pmtiles("places.pmtiles".to_string());
options.maxzoom = 12;
options.quiet = true;
let summary = manby::run(&options)?;
```

`Options` cannot be written as a struct literal, since each new option is a new field. As with tippecanoe, an existing tileset is an error unless `force` is set.

## Errors

An `Error` has tippecanoe's message for the problem and the exit status tippecanoe would use, so a program can exit as tippecanoe does.

```rust
if let Err(e) = manby::run(&options) {
    eprintln!("{}", e.message);
    std::process::exit(e.exit_code);
}
```

When a tile cannot be made small enough, `run` still succeeds, with `incomplete` set in its summary and a tileset that stops at `maxzoom`. tippecanoe exits with status 100 then.

Progress and warnings go to standard error as tippecanoe writes them, unless `quiet` is set.

## Joining

`join` is tile-join, with `JoinOptions` from its command line or as fields.

```rust
use manby::options::Output;

let mut join = manby::JoinOptions::default();
join.inputs = vec!["roads.pmtiles".to_string(), "places.pmtiles".to_string()];
join.output = Output::Pmtiles("map.pmtiles".to_string());
let summary = manby::join(&join)?;
println!("{} tiles, {} left out", summary.tiles, summary.skipped);
```

## Tile sizes

`stats::report` reads a tileset's tile sizes at each zoom, as `manby stats` prints them.

```rust
let report = manby::stats::report("map.pmtiles", false, 10)?;
for zoom in &report.zooms {
    println!("z{}: {} tiles, the largest {} bytes", zoom.z, zoom.tiles, zoom.max);
}
```

## One run at a time

Runs in one process should take turns unless their options are the same. The code manby borrows from tippecanoe-rs reads its options from globals, which each run sets, and the memory budget is the process's.

## What can change

The API is `run` and `join` with their options and summaries, and the `stats`, `serve` and `overzoom` modules. A change that breaks it comes with a new minor version while manby is 0.x. manby's other modules are public for its own tests and benchmarks, are hidden on docs.rs, and can change in any release.
