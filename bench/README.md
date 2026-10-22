# Benchmarks

`fetch-data.sh` downloads the data into `data/` and converts it to newline-delimited GeoJSON:

| File | What | Size |
| --- | --- | --- |
| `cb_2023_us_tract_500k.geojsonl` | 85,186 US census tracts (Census cartographic boundaries 2023) | 150 MB |
| `tl_2024_us_county.geojsonl` | 3,235 US counties at full resolution (TIGER/Line 2024) | 207 MB |
| `tl_2024_us_primaryroads.geojsonl` | 17,522 US primary roads (TIGER/Line 2024) | 93 MB |
| `points4m.geojsonl` | 4 million clustered random points (`points.py`) | 556 MB |

`run.sh` times manby against tippecanoe-rs, alternating the two, and checks the tiles match.
Build both in release mode first (`cargo build --release` in each repository).

`large.sh` runs manby with a memory budget (`BUDGET`, default 1G) on inputs several times
larger than it, which it makes on the first run: `points40m.geojsonl`, 40 million points
(5.6 GB), and `tracts20.geojsonl`, the tracts 20 times over, moved north and south by
`repeat.py` (3 GB). It reports wall time and peak resident memory, and checks the tiles
against tippecanoe-rs's.
