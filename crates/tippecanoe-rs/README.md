# tippecanoe-rs

A faithful Rust port of [tippecanoe](https://github.com/felt/tippecanoe) v2.82.0, which builds vector tilesets from GeoJSON, FlatGeobuf, CSV or Geobuf features.

It has the same six programs (`tippecanoe`, `tile-join`, `tippecanoe-decode`, `tippecanoe-enumerate`, `tippecanoe-overzoom` and `tippecanoe-json-tool`), the same options and the same messages, and it makes the same tiles.
The options are documented in the [tippecanoe README](https://github.com/felt/tippecanoe#readme).

## Building

```sh
cargo build --release
```

The programs end up in `target/release`. `make` also copies them to the repository root, where the test recipes expect them.

## Testing

```sh
make -j test
```

This runs tippecanoe's own test suite, with its recipes copied from the C++ Makefile, against the Rust programs, plus the unit tests through `cargo test`.
All 279 comparisons with tippecanoe's expected output pass except `allow-existing`, whose expected file is out of date upstream. The C++ build fails that test the same way on macOS and on Linux, and the Rust build matches the C++ output there exactly.

`make geobuf-test` also converts every fixture to Geobuf with a Rust port of the `json2geobuf` tool from the npm `geobuf` package (`examples/json2geobuf.rs`) and tiles that.

## Performance

About the same as the C++, with somewhat less memory. On an Apple M5 Max, with identical output:

| Input | C++ | Rust |
| --- | --- | --- |
| 4 million points, `-zg --drop-densest-as-needed`, PMTiles | 57.5 s, 2.3 GB | 59.0 s, 1.7 GB |
| 85,186 census tracts, `-zg --coalesce-densest-as-needed`, PMTiles | 28.8 s | 25.6 s |
| Census tracts at `-z12`, 312,073 tiles, PMTiles | 54.6 s | 49.5 s |
| Primary roads at `-z14`, 122,725 tiles, PMTiles | 14.9 s | 12.1 s |

## How it is organized

There is one Rust module per C++ source file, with the same function names and order, so the two can be read side by side.
The libraries tippecanoe vendors are ported as well. Wagyu and Clipper2 are separate crates in `crates/`.
[PORTING.md](PORTING.md) has the translation rules.

## Known differences

- Strings that are not valid UTF-8 become U+FFFD replacement characters, because Rust strings cannot hold them. tippecanoe passes such bytes through.
- Option errors use glibc's `getopt_long` wording on every platform, the wording of the Linux build that made the expected test output.
- The floating point code matches the C++ compiled without fused multiply-add, except for two calculations in `tile.rs` where it follows what clang emits on arm64.

## License

BSD 2-Clause, like tippecanoe, see [LICENSE.md](LICENSE.md). The Wagyu port is under the ISC license and the Clipper2 port under the Boost Software License, see their crates.
