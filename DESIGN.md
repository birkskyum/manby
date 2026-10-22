# manby design

manby builds vector tilesets from geographic features, like tippecanoe, but several times faster, ideally ten times.
It is named after the Aaron Manby, the first iron steamship.

## Baseline

tippecanoe and its faithful Rust port, [tippecanoe-rs](../tippecanoe-rs), run at about the same speed.
On an Apple M5 Max (18 cores, 128 GB):

| Benchmark | Command (after `tippecanoe -f -o out.pmtiles`) | tippecanoe | Target |
| --- | --- | --- | --- |
| points | `-zg --drop-densest-as-needed -P points4m.geojsonl` | 57.5 s | 6 s |
| tracts | `-zg --coalesce-densest-as-needed -P cb_2023_us_tract_500k.geojsonl` | 28.8 s | 3 s |
| tracts-z12 | `-z12 -Z0 --coalesce-densest-as-needed -P cb_2023_us_tract_500k.geojsonl` | 54.6 s | 5.5 s |
| roads-z14 | `-z14 -Z0 --drop-densest-as-needed -P tl_2024_us_primaryroads.geojsonl` | 14.9 s | 1.5 s |

`bench/` has the scripts and describes the data.

## Where tippecanoe spends its time

Measured from tippecanoe's own progress output:

- **Low zooms on few cores.** For the 4 million points, zoom levels 0 to 2 take 39 of the 57 seconds. Those levels hold 21 tiles, and tippecanoe works on each tile with a single thread.
- **Size limit retries.** Tiles over 500 KB are rebuilt with more features dropped, about 85 times for the points and 5 times for the tracts' single zoom 0 tile. Each retry reads all of the tile's features back from a compressed temporary file.
- **Idle cores.** Averaged over a run, tippecanoe keeps 2 to 3 of the 18 cores busy.
- **Zoom by zoom.** Every zoom level finishes before the next starts, and all features pass through zlib-compressed temporary files once per zoom.
- **PMTiles by way of SQLite.** PMTiles output is written as a complete MBTiles database first, checked with `PRAGMA integrity_check`, copied into a temporary file, and copied once more into the archive. That adds 22 to 31% on runs with hundreds of thousands of tiles.
- **Reading is not the problem.** Parsing 556 MB of GeoJSON in parallel takes about 4 seconds, and sorting about 4 more.

## Principles

1. **The same tiles, first.** In its default mode manby makes the tiles tippecanoe makes, for every option it supports, so the decoded output matches tippecanoe-rs exactly. The speed comes from how the work is done, not from changing what is computed. This keeps tippecanoe's 279 expected outputs and tippecanoe-rs as the referee while the engine underneath is replaced. Speedups that change the output are opt-in (see Fast mode) and are measured separately.
2. **A library first.** manby's own code has no global state and never exits the process. An `Options` value goes in and a `Result` comes out, so manby can run inside servers, other tools and, later, WebAssembly. The `manby` command is a thin layer that accepts tippecanoe's options.
3. **Deterministic in parallel.** The output does not depend on the number of threads or on scheduling. Tiles are keyed by z/x/y, the features of a tile are always in the same order, and parallel work inside a tile is collected in order.
4. **Memory first.** Features live in memory in compact fixed-size records. Inputs larger than memory can later move those arrays into memory-mapped files without changing the code that reads them.
5. **Same results on every platform.** No result depends on unstable sorts, fused multiply-add or other platform differences. Where tippecanoe's own output depends on the platform, manby follows the Linux build that made the expected test output.

## Architecture

```
input files ──► ingest ──► FeatureStore ──► order ──► tiler ──► TileSink ──► .pmtiles / .mbtiles / directory
               (parallel)                  (sort, minzoom)   (quadtree, parallel)  (streaming)
```

### Ingest (`src/ingest/`)

The input is split into byte ranges at line or feature boundaries, and each thread parses its range into its own part of the store, with its own interned keys and values. The parts are then joined and the interned ids remapped. Parsing follows tippecanoe's rules for what counts as a feature and how attribute values are typed and written, by reusing tippecanoe-rs's `jsonpull`, `read_json` and `serial` code where that is fast enough, or a faster parser where it is not.

### FeatureStore (`src/store.rs`)

All features as fixed-size `Feature` records, with their geometry and attributes in shared arrays. Keys and values are interned once. A record holds what tippecanoe serializes for a feature (index, sequence, bbox, extent, label point, id, layer, type, minimum and maximum zoom).

### Order (`src/order.rs`)

Sorts the records by tippecanoe's sort order with a parallel radix sort, then makes the single ordered pass tippecanoe makes to assign each feature its minimum zoom from the drop rate and base zoom. It also collects what `-zg` and `-Bg` need to guess the zoom levels.

### Tiler (`src/tiler/`)

Walks the tile quadtree from the top. Each tile is a task. Its input is the features its parent passed down (clipped to the parent tile, as in tippecanoe), and its outputs are the finished tile and the clipped features for its children. Children start as soon as their parent is done, so all cores stay busy without waiting for whole zoom levels. A tile with many features splits its per-feature work (clipping, simplification, polygon cleaning, encoding) across threads and collects the results in order. When a tile is over the size or feature limit, the retry repeats only the selection and encoding of data already in memory, choosing the next threshold exactly as tippecanoe does, so the same tiles come out. The geometry algorithms come from tippecanoe-rs (`clip`, `geometry`, `visvalingam`, `shared_borders`, `wagyu`, `mvt`).

### Sinks (`src/sink/`)

`TileSink` receives finished tiles from any thread.
- **PMTiles.** Writes directly, without SQLite. Tiles are deduplicated by content hash, kept in memory or appended to a spill file, laid out in tile id order at the end so the archive is clustered, and the directories are built with tippecanoe-rs's `pmtiles` module. The data is written once.
- **MBTiles.** One writer thread inserting in large transactions.
- **Directory.** Parallel file writes.

### Compression (`src/compress.rs`)

In the default mode tiles are compressed with the system zlib at tippecanoe's settings, because the size limit is checked on the compressed size and so the compressed bytes decide which features are dropped. Fast mode uses libdeflate.

## Fast mode

Opt-in changes that make different tiles, measured against the default mode for speed and for how close the tiles stay (tile sizes under the limits, features kept per zoom):
- libdeflate instead of zlib.
- Predicting each feature's share of the tile size and choosing the drop threshold in one pass, instead of encode-and-retry.
- Reading the features of a low zoom tile in drop-priority order, so a zoom 0 tile touches only the features it keeps.

## MapLibre Tiles (MLT)

`--tile-format=mlt` writes MapLibre Tiles instead of Mapbox Vector Tiles. It is manby's first output beyond tippecanoe, so it is opt-in, and the default output does not change.
- **Encoding.** The tiler makes each tile's MVT bytes as usual, and `src/mlt.rs` turns them into MLT version 1 layers with mlt-core's `LayerWriter`, before the tile is compressed and its size checked. An MLT tile holds the layers, features (in the same order), ids, geometry and attribute values of the MVT tile. manby reads the MVT bytes itself, because mlt-core's reader refuses tiles that decode to more than 32 MiB and tippecanoe allows 200,000 features in a tile. `examples/mlt_reader_check.rs` checks that both make the same bytes.
- **Attribute types.** MLT stores each attribute of a layer as one typed column, so attributes are typed as mlt-core's own MVT conversion types them. Integers of different MVT types become one integer column, integers mixed with floats become 64-bit floats, and any other mix (strings with numbers, booleans with numbers) becomes strings, in mlt-core's text form. The order of attributes within a feature is the column order, not the feature's own.
- **Encoder profile.** `--mlt-profile` picks mlt-core's encoder defaults. The default, `web`, writes tiles every MapLibre client reads and leaves out polygon triangles, which MapLibre GL JS ignores. `native` stores triangles where they save MapLibre Native more time than their bytes cost, `min-size` writes the fewest bytes and may reorder the features of a layer (and so their drawing order), and `fast` encodes quickest.
- **Compression.** As for MVT, tiles are gzipped unless `-pC`, which is tippecanoe's rule and Planetiler's default for MLT too. The encoder is told which, because FastPFOR and FSST make tiles smaller on their own but larger once gzipped, so they are off under gzip. On the four benchmarks gzipped MLT is 2 to 45% smaller than gzipped MVT, and smaller than MLT without gzip. MapLibre's `mlt` CLI writes MLT without gzip by default, and its `ls` reads only uncompressed files.
- **Outputs.** PMTiles archives get tile type 6 (MapLibre Vector Tile in the PMTiles spec) and `"format": "mlt"` in their metadata, MBTiles databases the `format` "mlt", and directories `z/x/y.mlt` files and `"format": "mlt"` in metadata.json, the values Martin and the `mlt` CLI use. MapLibre GL JS (5.12 and later) reads the tiles from a vector source with `encoding: "mlt"`.
- **Size limit.** The tile size limit (`-M`, 500 KB by default) is measured on the stored MLT bytes, as tippecanoe measures the stored MVT bytes. MLT tiles are smaller, so a tile that reaches the limit can keep more features than the MVT tile would, and the as-needed strategies drop and coalesce less. Tiles under the limits hold the same features as the MVT tiles.
- **Rust version.** mlt-core needs Rust 1.98, so MLT output is behind the `mlt` cargo feature (`cargo +1.98.1 build --release --features mlt`). Without it, `--tile-format=mlt` fails with an error that says so.
- **Verification.** `examples/mlt_verify.rs` decodes the MLT archive of a run with mlt-core and the MVT archive of the same run with tippecanoe-rs, and compares every tile, layer, feature, id, geometry part and attribute value under the typing rules above. `make test-golden-mlt` runs it on the golden tests.

## Library and command

`manby::run(&Options) -> Result<Summary>` is the entry point. Transitionally, the algorithm functions borrowed from tippecanoe-rs read tippecanoe-rs's globals. manby sets those once from `Options` and replaces each borrowed function with its own parameterized copy when it needs to change it.

## Verification

- **Expected outputs.** `make test-golden` runs tippecanoe's `.json.check` recipes with `manby` for the options it supports. A skip list names the rest and shrinks over time.
- **Against tippecanoe-rs.** The benchmark script compares manby's decoded output with tippecanoe-rs's for every benchmark, every time.
- **Unit tests** for each module, and determinism tests that run the same input with 1, 2 and 18 threads.

## Milestones

1. **Points.** GeoJSON points to PMTiles in the default mode, identical to tippecanoe on the points benchmark, at least 5 times faster.
2. **Lines and polygons.** The tracts, tracts-z12 and roads-z14 benchmarks identical and at least 5 times faster, including coalescing and polygon cleaning.
3. **Options.** Most of tippecanoe's options, MBTiles and directory output, FlatGeobuf and CSV input, most golden tests passing.
4. **Beyond tippecanoe.** Fast mode, MapLibre Tiles (MLT) output, input through terrio (GeoPackage, GeoParquet, Shapefile), WebAssembly.
