---
title: Joining tilesets
description: manby join is tippecanoe's tile-join, with the same tiles, two to thirty-five times faster.
---

`manby join` takes tile-join's options, prints its messages, exits as it does, and makes the same tiles, with their features in the same order. It merges tilesets, joins attributes from a CSV file, filters and renames layers, filters features, overzooms, and converts between MBTiles, PMTiles and directories.

```sh
manby join -o merged.pmtiles roads.pmtiles places.mbtiles
manby join -c tracts.csv -o joined.pmtiles tracts.pmtiles
```

`manby tile-join` works too. Under the name `tile-join`, a link to it for example, manby is tile-join, so it can take its place.

## Speed

Median of three runs, against tile-join 2.82.0. For every run the tiles decode to exactly what tile-join makes.

| Benchmark | System | tile-join | manby join | Speedup | tile-join memory | manby join memory |
| --- | --- | --: | --: | --: | --: | --: |
| Copy to PMTiles | macOS | 21.9 s | 0.63 s | 34.8x | 589 MB | 161 MB |
| | Linux | 24.2 s | 1.05 s | 23.1x | 461 MB | 185 MB |
| Copy to MBTiles | macOS | 16.6 s | 1.19 s | 13.9x | 560 MB | 380 MB |
| | Linux | 20.4 s | 1.28 s | 16.0x | 463 MB | 371 MB |
| Join a CSV | macOS | 22.6 s | 1.09 s | 20.7x | 652 MB | 296 MB |
| | Linux | 25.9 s | 1.92 s | 13.5x | 556 MB | 325 MB |
| Merge to PMTiles | macOS | 1.20 s | 0.38 s | 3.2x | 701 MB | 269 MB |
| | Linux | 1.62 s | 0.68 s | 2.4x | 573 MB | 249 MB |
| Merge to MBTiles | macOS | 1.15 s | 0.40 s | 2.9x | 707 MB | 273 MB |
| | Linux | 1.51 s | 0.69 s | 2.2x | 567 MB | 258 MB |
| Overzoom | macOS | 3.87 s | 0.39 s | 9.9x | 592 MB | 182 MB |
| | Linux | 5.99 s | 0.72 s | 8.3x | 440 MB | 196 MB |

The copies take the census tracts at `-z12` (312,073 tiles), and the CSV join joins them to a CSV keyed on `GEOID`. The merges take the 4 million points and the tracts, both at `-zg`. The overzoom takes counties at `-z5` and tracts at `-z9`, with `--overzoom`. The [benchmarks](../../reference/benchmarks/) describe the machines.

The merges spend most of their time on a hundred tiles near the 500 KB limit, which manby join, like tile-join, has to compress with zlib to check their size.

## How

tile-join decodes each tile into layers that each allocate a megabyte of hash tables, does its overzooming one tile at a time, compresses every tile with zlib, and writes PMTiles by way of a SQLite database. manby join keeps tile-join's order of input tiles and its metadata, does the work on the tiles on every core without those tables, keeps the compression of tiles it does not change, and writes PMTiles directly.

## Tiles that several inputs share

Where several inputs have a tile at the same z/x/y, tile-join appends their features in the order of the tiles' stored bytes. manby compresses most tiles with libdeflate, whose bytes sort differently from zlib's, so for tilesets that manby made, manby join compares tiles by the bytes zlib makes of them, which are tippecanoe's. So manby and then manby join make the tiles that tippecanoe and then tile-join make. tile-join itself, run on manby's tilesets, can order the features of those shared tiles differently. Tilesets that manby made with `--compression=zlib` are joined the same way by both.
