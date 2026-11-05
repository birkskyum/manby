# Benchmarks

`fetch-data.sh` downloads the data into `data/` and converts it to newline-delimited GeoJSON:

| File | What | Size |
| --- | --- | --- |
| `cb_2023_us_tract_500k.geojsonl` | 85,186 US census tracts (Census cartographic boundaries 2023) | 150 MB |
| `tl_2024_us_county.geojsonl` | 3,235 US counties at full resolution (TIGER/Line 2024) | 207 MB |
| `tl_2024_us_primaryroads.geojsonl` | 17,522 US primary roads (TIGER/Line 2024) | 93 MB |
| `points4m.geojsonl` | 4 million clustered random points (`points.py`) | 556 MB |

`run.sh` times manby against tippecanoe, alternating the two, with wall time and peak resident
memory, and checks the tiles match. `points-cluster` clusters the points within 10 pixels and sums
their `value` attribute
(`-zg -r1 --cluster-distance=10 --cluster-densest-as-needed --accumulate-attribute=value:sum`);
`cluster-densest` (`-zg --cluster-densest-as-needed`) and `cluster-r1-sum` (the same with `-r1`
and the summed `value`) run only when named. The reference is tippecanoe-rs, or tippecanoe's C++ build with
`TIPPECANOE=../tippecanoe/tippecanoe` (and `TILE_JOIN` for `join.sh`). Build them in release mode
first (`cargo build --release --workspace`, or `make` for the C++). `OUT` keeps the tilesets of runs against
different references apart.

`large.sh` runs manby with a memory budget (`BUDGET`, default 1G) on inputs several times
larger than it, which it makes on the first run: `points40m.geojsonl`, 40 million points
(5.6 GB), and `tracts20.geojsonl`, the tracts 20 times over, moved north and south by
`repeat.py` (3 GB). It reports wall time and peak resident memory, and checks the tiles
against tippecanoe's. Each runs once, so on a laptop `PAUSE` (seconds) lets the machine cool down
before each run.

`join.sh` times `manby join` against tile-join, alternating the two, with wall
time and peak resident memory, and checks that the tiles decode the same. It copies the tracts
at `-z12` to PMTiles and to MBTiles, joins them to a CSV keyed on `GEOID` (`tracts-csv.py`),
merges the points and the tracts (both `-zg`) to PMTiles and to MBTiles, and merges counties at
`-z5` with tracts at `-z9` with `--overzoom`. It makes its inputs in `out/join` on the first run,
with manby for the copies and the CSV join, and with tippecanoe for the merges, since the order
of features in tiles that several inputs share depends on the bytes of the stored tiles. For the
merges it also checks manby, then `manby join`, against tippecanoe, then tile-join.

## Linux

The Linux numbers in the README come from a Lenovo Yoga Slim 7x (Snapdragon X Elite, 12 cores,
32 GB) running Arch Linux ARM, with the output and the data on its btrfs home filesystem and `/tmp`
in memory. tippecanoe, tile-join and manby were built for it in a Debian 12 arm64 container, from
the `rust:1.89-bookworm` image with `libsqlite3-dev` and `zlib1g-dev` added, and copied over with
GNU `time` from the same image. The large inputs were made there with `points.py` and
`repeat.py`, and match the Mac's byte for byte.

`MANBY_ONLY=1` runs only manby, against the reference's tilesets that an earlier run of the
reference left in `OUT`, so a new manby can be measured without waiting for tippecanoe again.
The README's figures for tippecanoe and tile-join come from such earlier runs on the same
machines.

tippecanoe's arm64 builds (Apple's clang on macOS, GCC on Linux) fuse multiplications and
additions, where its x86-64 build does not, and so make a few tiles near the size limit
differently. Of these benchmarks, that happens with the tracts at `-zg` (`tracts`, `tracts20`, and
the merges made from them). Built for x86-64, tippecanoe makes manby's tiles on `points`,
`tracts`, `tracts-z12` and `roads-z14`, and built with `CXXFLAGS=-ffp-contract=off` it makes them
on `tracts` and `tracts20`, and tile-join then makes `manby join`'s merge.
