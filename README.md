# manby

manby builds vector tilesets from GeoJSON, FlatGeobuf, CSV or Geobuf features. It makes the same tiles as [tippecanoe](https://github.com/felt/tippecanoe), ten to seventeen times faster on a Mac and six to fourteen times faster on Linux, in less memory. `manby join` is tippecanoe's tile-join, two to thirty-five times faster.

The `manby` command takes tippecanoe's options:

```sh
manby -zg --drop-densest-as-needed -o places.pmtiles places.geojson
```

## Speed

Against tippecanoe 2.82.0 itself, the C++ built with its own Makefile, writing PMTiles, median of three alternating runs. The memory figures are peak resident memory.

On macOS, on an Apple M5 Max with 18 cores:

| Benchmark | tippecanoe | manby | |
| --- | --- | --- | --- |
| 4 million points, `-zg --drop-densest-as-needed` | 61.4 s, 2.22 GB | 3.6 s, 1.34 GB | 17.0x |
| The same points, `-zg -r1 --cluster-distance=10 --cluster-densest-as-needed --accumulate-attribute=value:sum` | 54.4 s, 1.29 GB | 3.8 s, 0.90 GB | 14.2x |
| The same points, `-zg --cluster-densest-as-needed` | 59.8 s, 2.28 GB | 3.7 s, 1.33 GB | 16.2x |
| The same points, `-zg -r1 --cluster-densest-as-needed --accumulate-attribute=value:sum` | 87.7 s, 3.36 GB | 7.2 s, 2.07 GB | 12.1x |
| 17,522 roads at `-z14`, 122,725 tiles | 11.8 s, 0.32 GB | 0.86 s, 0.22 GB | 13.7x |
| 85,186 census tracts at `-z12`, 312,073 tiles | 46.6 s, 0.55 GB | 4.1 s, 0.40 GB | 11.5x |
| 85,186 census tracts, `-zg --coalesce-densest-as-needed` | 24.8 s, 0.54 GB | 2.3 s, 0.36 GB | 10.8x |

On Linux (Debian 12, arm64), in a virtual machine on the same Mac, with 16 of its cores:

| Benchmark | tippecanoe | manby | |
| --- | --- | --- | --- |
| 4 million points, `-zg --drop-densest-as-needed` | 46.4 s, 1.64 GB | 3.6 s, 1.18 GB | 12.8x |
| The same points, clustered within 10 pixels, summing `value` | 40.9 s, 1.08 GB | 3.0 s, 0.87 GB | 13.8x |
| The same points, `-zg --cluster-densest-as-needed` | 47.5 s, 1.73 GB | 3.9 s, 1.20 GB | 12.3x |
| The same points, `-zg -r1 --cluster-densest-as-needed`, summing `value` | 74.0 s, 2.62 GB | 7.7 s, 1.94 GB | 9.7x |
| 17,522 roads at `-z14`, 122,725 tiles | 8.5 s, 0.35 GB | 1.3 s, 0.20 GB | 6.5x |
| 85,186 census tracts at `-z12`, 312,073 tiles | 37.2 s, 0.58 GB | 4.9 s, 0.37 GB | 7.6x |
| 85,186 census tracts, `-zg --coalesce-densest-as-needed` | 23.9 s, 0.59 GB | 2.7 s, 0.32 GB | 8.8x |

tippecanoe runs faster on Linux than on macOS, and manby slower in the virtual machine on the runs with many small tiles, since its threads hand those tiles to each other often and a thread takes longer to wake there. A Linux machine of its own may differ. `bench/` has the data, the scripts and how the Linux numbers were made.

For every run the tiles decode to exactly what tippecanoe makes, with one exception. tippecanoe's arm64 builds, from Apple's clang on macOS and from GCC on Linux, fuse multiplications and additions where its x86-64 build does not, and that changes a few tiles near the size limit. On these benchmarks it changes only the census tracts at `-zg`, here and 20 times over below. In the tracts, the zoom 0 tile comes out 6 bytes smaller compressed, so that tippecanoe's retry keeps other features. manby makes the same tiles on macOS and Linux alike, the tiles of tippecanoe's x86-64 build. Built for x86-64, or on arm64 with `-ffp-contract=off`, tippecanoe makes manby's tiles on the tracts too.

[tippecanoe-rs](../tippecanoe-rs), the faithful Rust port that manby takes tippecanoe's algorithms from, runs at about the speed of the C++.

The archives are not tippecanoe's byte for byte, and a little smaller. zlib's exact output decides which features a tile keeps only when the tile could be near the size limit. Every other tile is compressed with libdeflate, which is faster and compresses tiles better. `--compression=zlib` compresses every tile with zlib, as tippecanoe does, for the same archive bytes.

## How

tippecanoe spends most of its time waiting on itself rather than computing. It works through the low zoom levels one tile per thread, rebuilds oversized tiles by reading their features back from compressed temporary files, finishes each zoom level before starting the next, and writes PMTiles by way of a SQLite database. manby keeps the features in memory, sorted once, walks the tile pyramid in parallel on every core, splits big tiles across threads, retries oversized tiles on data already in memory, and writes PMTiles directly. The geometry algorithms are tippecanoe's own, by way of tippecanoe-rs, which is why the tiles come out the same. [DESIGN.md](DESIGN.md) has the measurements and the architecture.

## Memory

manby's peak memory is below tippecanoe's on the benchmarks. For inputs larger than memory, `--memory-budget` bounds it (default: half the physical memory, as tippecanoe sizes its sorting). Past the budget, the features and the tile data live in an unlinked temporary file in the `-t` directory, mapped into memory, laid out in the order the tiling reads them, and manby keeps its resident memory near the budget. `bench/large.sh` tiles inputs several times larger than a 1 GB budget, once each:

| Input | System | tippecanoe | manby, `--memory-budget=1G` |
| --- | --- | --- | --- |
| 40 million points (5.6 GB), `-zg --drop-densest-as-needed` | macOS | 652 s, 9.97 GB | 103 s, 1.05 GB |
| | Linux | 674 s, 7.96 GB | 87 s, 1.06 GB |
| The census tracts 20 times over (3 GB), `-zg --coalesce-densest-as-needed` | macOS | 564 s, 4.70 GB | 151 s, 1.05 GB |
| | Linux | 534 s, 4.17 GB | 172 s, 0.99 GB |

## Joining tilesets

`manby join` takes tile-join's options, prints its messages, exits as it does, and makes the same tiles, with their features in the same order. It merges tilesets, joins attributes from a CSV file, filters and renames layers, filters features, overzooms, and converts between MBTiles, PMTiles and directories:

```sh
manby join -o merged.pmtiles roads.pmtiles places.mbtiles
manby join -c tracts.csv -o joined.pmtiles tracts.pmtiles
```

`manby tile-join` works too. Under the name `tile-join`, a link to it for example, manby is tile-join, so it can take its place.

On the same machines, median of three alternating runs, against tile-join 2.82.0, the C++. For every run the tiles decode to exactly what tile-join makes.

| Benchmark | System | tile-join | manby join | |
| --- | --- | --- | --- | --- |
| The tracts at `-z12` (312,073 tiles) copied to PMTiles | macOS | 21.9 s, 0.58 GB | 0.62 s, 0.16 GB | 35.3x |
| | Linux | 9.9 s, 0.51 GB | 0.65 s, 0.14 GB | 15.2x |
| The same, to MBTiles | macOS | 16.6 s, 0.55 GB | 1.22 s, 0.37 GB | 13.6x |
| | Linux | 7.5 s, 0.51 GB | 1.33 s, 0.32 GB | 5.6x |
| The same tracts joined to a CSV keyed on `GEOID` | macOS | 22.6 s, 0.64 GB | 1.05 s, 0.29 GB | 21.5x |
| | Linux | 10.3 s, 0.62 GB | 1.17 s, 0.27 GB | 8.8x |
| The points and the tracts, both `-zg`, merged to PMTiles | macOS | 1.20 s, 0.68 GB | 0.38 s, 0.25 GB | 3.2x |
| | Linux | 1.24 s, 0.61 GB | 0.42 s, 0.21 GB | 3.0x |
| The same, to MBTiles | macOS | 1.15 s, 0.69 GB | 0.47 s, 0.26 GB | 2.4x |
| | Linux | 1.13 s, 0.61 GB | 0.42 s, 0.21 GB | 2.7x |
| Counties at `-z5` and tracts at `-z9`, with `--overzoom` | macOS | 3.87 s, 0.58 GB | 0.34 s, 0.18 GB | 11.4x |
| | Linux | 7.41 s, 0.52 GB | 0.44 s, 0.15 GB | 16.8x |

The merges spend most of their time on a hundred tiles near the 500 KB limit, which manby join, like tile-join, has to compress with zlib to check their size. `bench/join.sh` has the benchmarks.

tile-join decodes each tile into layers that each allocate a megabyte of hash tables, does its overzooming one tile at a time, compresses every tile with zlib, and writes PMTiles by way of a SQLite database. manby join keeps tile-join's order of input tiles and its metadata, does the work on the tiles on every core without those tables, keeps the compression of tiles it does not change, and writes PMTiles directly. [DESIGN.md](DESIGN.md) has the details.

Where several inputs have a tile at the same z/x/y, tile-join appends their features in the order of the tiles' stored bytes. manby compresses most tiles with libdeflate, whose bytes sort differently from zlib's, so for tilesets that manby made, manby join compares tiles by the bytes zlib makes of them, which are tippecanoe's. So manby and then manby join make the tiles that tippecanoe and then tile-join make, with the tiles of tippecanoe's x86-64 build as above. tile-join itself, run on manby's tilesets, can order the features of those shared tiles differently. Tilesets that manby made with `--compression=zlib` are joined the same way by both.

## tippecanoe's other programs

manby also runs tippecanoe's smaller programs, as tippecanoe-rs has them, so one program does all of tippecanoe's work:

| tippecanoe | manby |
| --- | --- |
| `tippecanoe` | `manby` |
| `tile-join` | `manby join` |
| `tippecanoe-decode` | `manby decode` |
| `tippecanoe-enumerate` | `manby enumerate` |
| `tippecanoe-overzoom` | `manby overzoom` |
| `tippecanoe-json-tool` | `manby json-tool` |

Each program's own name works as the subcommand too (`manby tippecanoe-decode`), and under that name, through a link, manby is that program, so links named after all six make manby a drop-in replacement for tippecanoe. Only the tiling and joining are manby's own, since that is where the time goes. The other four are tippecanoe-rs's code and run at its speed.

To tile an input file named like a subcommand, such as `decode`, write it as `./decode`.

## Building and testing

manby uses tippecanoe-rs as a library, so it expects that repository next to this one, at `../tippecanoe-rs`.

```sh
cargo build --release
make test
```

`make test` runs the unit tests and tippecanoe's own test suite with manby in place of tippecanoe, from GeoJSON input and again from Geobuf input. All 188 of tippecanoe's expected outputs match, and the tests of tippecanoe's other programs pass with manby as each of them.

manby is a library first. `manby::run(&Options)` reads, tiles and writes a tileset and returns an error instead of exiting, `manby::join(&JoinOptions)` joins tilesets, and the commands are thin layers over them.

## MapLibre Tiles

`--tile-format=mlt` writes [MapLibre Tiles](https://maplibre.org/maplibre-tile-spec/) instead of Mapbox Vector Tiles, with the same features, into PMTiles, MBTiles or a directory. The tiles are 2 to 45% smaller than MVT on the benchmarks, and MapLibre GL JS 5.12 and later reads them from a vector source with `encoding: "mlt"`. The encoder, mlt-core, needs Rust 1.98, so MLT output is a cargo feature.

```sh
cargo +1.98.1 build --release --features mlt
manby -zg --drop-densest-as-needed --tile-format=mlt -o places.pmtiles places.geojson
```

DESIGN.md describes the choices behind the tiles, and `make test-golden-mlt` checks them against the MVT tiles of the same runs.

## License

BSD 2-Clause, see [LICENSE.md](LICENSE.md).
