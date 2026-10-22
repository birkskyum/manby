# manby

manby builds vector tilesets from GeoJSON, FlatGeobuf, CSV or Geobuf features. It makes the same tiles as [tippecanoe](https://github.com/felt/tippecanoe), ten to sixteen times faster.

The `manby` command takes tippecanoe's options:

```sh
manby -zg --drop-densest-as-needed -o places.pmtiles places.geojson
```

## Speed

On an Apple M5 Max with 18 cores, average of two runs, writing PMTiles. For every run the tiles decode to exactly what tippecanoe makes.

| Benchmark | tippecanoe | manby | |
| --- | --- | --- | --- |
| 4 million points, `-zg --drop-densest-as-needed` | 59.7 s | 3.7 s | 16x |
| 17,522 roads at `-z14`, 122,725 tiles | 10.0 s | 0.72 s | 14x |
| 85,186 census tracts at `-z12`, 312,073 tiles | 51.2 s | 3.8 s | 13.5x |
| 85,186 census tracts, `-zg --coalesce-densest-as-needed` | 25.1 s | 2.5 s | 10x |

The tippecanoe column is [tippecanoe-rs](../tippecanoe-rs), the faithful Rust port, which runs at the same speed as the C++. `bench/` has the data and the script.

## How

tippecanoe spends most of its time waiting on itself rather than computing. It works through the low zoom levels one tile per thread, rebuilds oversized tiles by reading their features back from compressed temporary files, finishes each zoom level before starting the next, and writes PMTiles by way of a SQLite database. manby keeps the features in memory, sorted once, walks the tile pyramid in parallel on every core, splits big tiles across threads, retries oversized tiles on data already in memory, and writes PMTiles directly. The geometry algorithms are tippecanoe's own, by way of tippecanoe-rs, which is why the tiles come out the same. [DESIGN.md](DESIGN.md) has the measurements and the architecture.

## Building and testing

manby uses tippecanoe-rs as a library, so it expects that repository next to this one, at `../tippecanoe-rs`.

```sh
cargo build --release
make test
```

`make test` runs the unit tests and tippecanoe's own test suite with manby in place of tippecanoe, from GeoJSON input and again from Geobuf input. All 188 of tippecanoe's expected outputs match.

manby is a library first. `manby::run(&Options)` reads, tiles and writes a tileset and returns an error instead of exiting, and the command is a thin layer over it.

## License

BSD 2-Clause, see [LICENSE.md](LICENSE.md).
