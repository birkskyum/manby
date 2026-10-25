---
title: Getting started
description: Building manby, and making a first tileset with tippecanoe's options.
---

manby is not on crates.io yet, so for now it builds from source, with Rust 1.89 or later. It uses [tippecanoe-rs](https://github.com/birkskyum/tippecanoe-rs), the faithful Rust port of tippecanoe, as a library, and expects that repository next to its own.

```sh
git clone https://github.com/birkskyum/tippecanoe-rs
git clone https://github.com/birkskyum/manby
cd manby
cargo build --release
```

The program is `target/release/manby`.

## A first tileset

manby takes [tippecanoe's options](https://github.com/felt/tippecanoe#readme), and a tippecanoe command works with `manby` in its place.

```sh
manby -zg --drop-densest-as-needed -o places.pmtiles places.geojson
```

The input can be GeoJSON, newline-delimited GeoJSON (read in parallel with `-P`), FlatGeobuf, CSV or Geobuf. GeoPackage, GeoParquet, Shapefile and OpenStreetMap data can be [converted to FlatGeobuf](../guides/other-formats/) with GDAL first. The output is a PMTiles archive, an MBTiles database, or a directory of tiles with `-e`.

Points can be clustered as tippecanoe clusters them, with their attributes accumulated.

```sh
manby -zg -r1 --cluster-distance=10 --cluster-densest-as-needed \
  --accumulate-attribute=value:sum -o clusters.pmtiles points.geojsonl
```

## Testing

```sh
make test
```

`make test` runs the unit tests and tippecanoe's own test suite with manby in place of tippecanoe, from GeoJSON input and again from Geobuf input. All 188 of tippecanoe's expected outputs match, and the tests of tippecanoe's other programs pass with manby as each of them.

## As a library

manby is a library first. `manby::run(&Options)` reads, tiles and writes a tileset and returns an error instead of exiting, `manby::join(&JoinOptions)` joins tilesets, and the commands are thin layers over them.
