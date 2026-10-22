# manby

manby builds vector tilesets from GeoJSON, FlatGeobuf, CSV or Geobuf features. It makes the same tiles as [tippecanoe](https://github.com/felt/tippecanoe), eleven to eighteen times faster, in less memory.

The `manby` command takes tippecanoe's options:

```sh
manby -zg --drop-densest-as-needed -o places.pmtiles places.geojson
```

## Speed

On an Apple M5 Max with 18 cores, median of three alternating runs, writing PMTiles. For every run the tiles decode to exactly what tippecanoe makes.

| Benchmark | tippecanoe | manby | |
| --- | --- | --- | --- |
| 4 million points, `-zg --drop-densest-as-needed` | 56.6 s, 1.98 GB | 3.2 s, 1.37 GB | 17.7x |
| The same points, `-zg -r1 --cluster-distance=10 --cluster-densest-as-needed --accumulate-attribute=value:sum` | 51.5 s, 1.05 GB | 3.6 s, 0.91 GB | 14.2x |
| 17,522 roads at `-z14`, 122,725 tiles | 9.7 s, 0.31 GB | 0.83 s, 0.22 GB | 11.7x |
| 85,186 census tracts at `-z12`, 312,073 tiles | 46.8 s, 0.55 GB | 4.0 s, 0.41 GB | 11.8x |
| 85,186 census tracts, `-zg --coalesce-densest-as-needed` | 24.8 s, 0.56 GB | 2.2 s, 0.39 GB | 11.2x |

The memory figures are peak resident memory. manby also uses less processor time in total, a fifth of tippecanoe's on the points.

Clustering gains as much with tippecanoe's other options for it. On the same points, `-zg --cluster-densest-as-needed` takes 3.7 s and 1.35 GB against 57.2 s and 1.73 GB (15x), and `-zg -r1 --cluster-densest-as-needed --accumulate-attribute=value:sum` takes 7.0 s and 2.15 GB against 84.8 s and 3.74 GB (12x).

The tippecanoe column is [tippecanoe-rs](../tippecanoe-rs), the faithful Rust port, which runs at the same speed as the C++. `bench/` has the data and the script.

The archives are not tippecanoe's byte for byte, and a little smaller. zlib's exact output decides which features a tile keeps only when the tile could be near the size limit. Every other tile is compressed with libdeflate, which is faster and compresses tiles better. `--compression=zlib` compresses every tile with zlib, as tippecanoe does, for the same archive bytes.

## How

tippecanoe spends most of its time waiting on itself rather than computing. It works through the low zoom levels one tile per thread, rebuilds oversized tiles by reading their features back from compressed temporary files, finishes each zoom level before starting the next, and writes PMTiles by way of a SQLite database. manby keeps the features in memory, sorted once, walks the tile pyramid in parallel on every core, splits big tiles across threads, retries oversized tiles on data already in memory, and writes PMTiles directly. The geometry algorithms are tippecanoe's own, by way of tippecanoe-rs, which is why the tiles come out the same. [DESIGN.md](DESIGN.md) has the measurements and the architecture.

## Memory

manby's peak memory is below tippecanoe's on the benchmarks. For inputs larger than memory, `--memory-budget` bounds it (default: half the physical memory, as tippecanoe sizes its sorting). Past the budget, the features and the tile data live in an unlinked temporary file in the `-t` directory, mapped into memory, laid out in the order the tiling reads them, and manby keeps its resident memory near the budget. `bench/large.sh` tiles inputs several times larger than a 1 GB budget:

| Input | tippecanoe | manby, `--memory-budget=1G` |
| --- | --- | --- |
| 40 million points (5.6 GB), `-zg --drop-densest-as-needed` | 702 s, 8.1 GB | 100 s, 1.3 GB |
| The census tracts 20 times over (3 GB), `-zg --coalesce-densest-as-needed` | 655 s, 6.2 GB | 150 s, 1.0 GB |

## Building and testing

manby uses tippecanoe-rs as a library, so it expects that repository next to this one, at `../tippecanoe-rs`.

```sh
cargo build --release
make test
```

`make test` runs the unit tests and tippecanoe's own test suite with manby in place of tippecanoe, from GeoJSON input and again from Geobuf input. All 188 of tippecanoe's expected outputs match.

manby is a library first. `manby::run(&Options)` reads, tiles and writes a tileset and returns an error instead of exiting, and the command is a thin layer over it.

## MapLibre Tiles

`--tile-format=mlt` writes [MapLibre Tiles](https://maplibre.org/maplibre-tile-spec/) instead of Mapbox Vector Tiles, with the same features, into PMTiles, MBTiles or a directory. The tiles are 2 to 45% smaller than MVT on the benchmarks, and MapLibre GL JS 5.12 and later reads them from a vector source with `encoding: "mlt"`. The encoder, mlt-core, needs Rust 1.98, so MLT output is a cargo feature.

```sh
cargo +1.98.1 build --release --features mlt
manby -zg --drop-densest-as-needed --tile-format=mlt -o places.pmtiles places.geojson
```

DESIGN.md describes the choices behind the tiles, and `make test-golden-mlt` checks them against the MVT tiles of the same runs.

## License

BSD 2-Clause, see [LICENSE.md](LICENSE.md).
