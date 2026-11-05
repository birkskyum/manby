---
title: Why manby?
description: What manby is for, how it compares with tippecanoe and the other ways of making vector tiles, and when to use something else.
---

## The short answer

- **tippecanoe's tiles.** manby takes tippecanoe's options and makes the tiles tippecanoe makes. All 188 of tippecanoe's expected test outputs match, so a tippecanoe command works with `manby` in its place.
- **Much faster.** Eight to eighteen times faster on the [benchmarks](../reference/benchmarks/), and up to forty times where tippecanoe's disk slows it down.
- **Less memory.** manby's peak memory is below tippecanoe's on every benchmark, on macOS and on Linux.
- **Larger than memory.** With [`--memory-budget`](../guides/memory/), 40 million points take 79 seconds in 1.1 GB, where tippecanoe takes 652 seconds and 10 GB.
- **tile-join too.** [`manby join`](../guides/join/) makes tile-join's tiles, two to thirty-five times faster.
- **MapLibre Tiles.** [`--tile-format=mlt`](../guides/mlt/) writes MLT as well as Mapbox Vector Tiles.

## Compared with the alternatives

| Tool | Written in | Reads | Keeping tiles small | MapLibre Tiles |
| --- | --- | --- | --- | --- |
| tippecanoe | C++ | GeoJSON, FlatGeobuf, CSV, Geobuf | Drops, coalesces or clusters features evenly across each zoom level until its tiles fit | In review |
| <span class="whitespace-nowrap">tippecanoe-rs</span> | Rust | As tippecanoe | As tippecanoe, with its tiles | No |
| manby | Rust | As tippecanoe, and [other formats by way of GDAL](../guides/other-formats/) | As tippecanoe, with its tiles | Yes |
| tylertoo | Rust, also from Python | GeoParquet | A size limit on each tile on its own, with its own thinning and simplification, so not tippecanoe's tiles | No |
| planetiler | Java | OpenStreetMap, Shapefile, GeoPackage, GeoJSON, GeoParquet | A profile, in Java or YAML, decides what each zoom level keeps | Yes |
| GDAL (ogr2ogr) | C++ | Almost any vector format | Each tile on its own, at a lower resolution past 500 kB, then only its largest features | No |
| freestiler | Rust, from R and Python | Data frames, GeoParquet, DuckDB queries | A drop rate below a base zoom, clustering and coalescing, not fitted to a size limit | Experimental |
| geojson-vt | JavaScript | GeoJSON | Simplification, with tiles made in the browser as the map asks for them | No |

### tippecanoe

[tippecanoe](https://github.com/felt/tippecanoe) is the reference. It is a mature, well-tested program, first by Mapbox and now by Felt, and the way it thins features so that every tile fits, evenly across a zoom level, is what makes its tilesets look right at every zoom. manby keeps all of that. If tippecanoe is fast enough for you, there is no need to switch.

### tippecanoe-rs

Before manby, tippecanoe was ported to Rust line by line, as [tippecanoe-rs](https://github.com/birkskyum/manby/tree/main/crates/tippecanoe-rs), which lives in manby's repository. It passes tippecanoe's test suite, makes the same tiles, and runs at the speed of the C++. So porting tippecanoe to Rust has been tried, and it is no faster. manby's speed comes from doing the work differently, and its geometry comes from the port, which is why the tiles are the same.

### tylertoo

[tylertoo](https://github.com/geoparquet-io/tylertoo) tiles GeoParquet into PMTiles, from the command line or from Python, and can read from remote storage and split a build across machines. It takes tippecanoe as its model rather than its output. Each oversized tile gets a threshold of its own, where tippecanoe raises one for the whole zoom level, and it thins and simplifies in its own way, so its tiles are not tippecanoe's. Its own benchmarks have it 8.6 to 21 times faster than tippecanoe at its default settings, and 5.9 to 16 times with its thinning off, with more memory on three of four datasets. For GeoParquet whose tiles need not be tippecanoe's, it saves the conversion step.

### planetiler

[planetiler](https://github.com/onthegomap/planetiler) builds basemaps, OpenMapTiles above all, from OpenStreetMap and other sources, and makes the whole planet in hours on one machine. A profile says which features each zoom level keeps. It is the tool for a basemap. manby is for tiling your own features as they are.

### GDAL

GDAL's `ogr2ogr` writes vector tiles from anything GDAL reads, zoom levels 0 to 5 by default. Each tile is made on its own, so a tile over 500 kB is written at a lower resolution and then cut down to its largest features, and nothing keeps the density even across a zoom level. GDAL is also the usual way into manby for the formats manby does not read, converting them to FlatGeobuf first.

### freestiler

[freestiler](https://github.com/walkerke/freestiler) is a Rust tiler for R and Python. It works on data frames and DuckDB queries, with tippecanoe's drop rate and base zoom, clustering and coalescing, but does not drop features to fit a size limit. It fits best inside an R or Python session.

### geojson-vt

[geojson-vt](https://github.com/maplibre/geojson-vt) slices GeoJSON into tiles in the browser as the map asks for them, and MapLibre GL JS uses it for GeoJSON sources. For data small enough to send to the browser whole, no tileset is needed.

## When to use something else

- **A basemap from OpenStreetMap.** planetiler, or tilemaker, with a profile such as OpenMapTiles. manby tiles the features it is given.
- **Tiles that follow a live database.** PostGIS's `ST_AsMVT`, served by [Martin](https://maplibre.org/martin/), makes each tile when it is asked for.
- **Small GeoJSON in a web map.** MapLibre's GeoJSON source, with geojson-vt.
- **Data frames in R or Python.** freestiler.
- **GeoParquet, without tippecanoe's tiles.** tylertoo reads it directly, from remote storage too.
- **Small inputs.** They take seconds with tippecanoe too, and tippecanoe is the reference.

## When manby fits

- **tippecanoe's tiles, in a fraction of the time.** Tilesets rebuilt often, in CI or every night, or many of them.
- **Large inputs on a small machine.** `--memory-budget` keeps memory near a budget, at a fraction of tippecanoe's time.
- **PMTiles for hosting.** manby writes PMTiles directly, ready for a bucket or a CDN.
- **GeoPackage, GeoParquet, Shapefile or OpenStreetMap.** GDAL's `ogr2ogr`, or any converter like it, makes FlatGeobuf of them, which manby reads in parallel. [Other formats](../guides/other-formats/) shows how.
