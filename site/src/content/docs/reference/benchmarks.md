---
title: Benchmarks
description: manby against tippecanoe 2.82.0 on macOS and on Linux, with the same tiles, in time and peak memory.
---

Against tippecanoe 2.82.0 itself, the C++ built with its own Makefile, writing PMTiles. Each figure is the median of three runs, and memory is peak resident memory. tippecanoe's runs and manby's come from separate sessions on the same machine, since tippecanoe's do not change with manby's version. The [commands](#the-commands) are below, and the scripts and the data are in [`bench/`](https://github.com/birkskyum/manby/tree/main/bench).

## macOS

An Apple M5 Max with 18 cores and 128 GB.

| Benchmark | tippecanoe | manby | Speedup | tippecanoe memory | manby memory |
| --- | --: | --: | --: | --: | --: |
| Points | 61.4 s | 3.44 s | 17.8x | 2,276 MB | 919 MB |
| Points, clustered by distance | 54.4 s | 3.88 s | 14.0x | 1,318 MB | 891 MB |
| Points, clustered as needed | 59.8 s | 4.07 s | 14.7x | 2,330 MB | 916 MB |
| Points, all zooms, clustered and summed | 87.7 s | 7.82 s | 11.2x | 3,443 MB | 1,115 MB |
| Roads at z14 | 11.8 s | 0.81 s | 14.5x | 327 MB | 177 MB |
| Tracts at z12 | 46.6 s | 3.78 s | 12.3x | 566 MB | 338 MB |
| Tracts, coalesced as needed | 24.8 s | 2.27 s | 10.9x | 554 MB | 366 MB |

## Linux

A Lenovo Yoga Slim 7x with a Snapdragon X Elite (12 cores, 32 GB), running Arch Linux ARM.

| Benchmark | tippecanoe | manby | Speedup | tippecanoe memory | manby memory |
| --- | --: | --: | --: | --: | --: |
| Points | 88.5 s | 5.86 s | 15.1x | 1,088 MB | 994 MB |
| Points, clustered by distance | 82.5 s | 6.02 s | 13.7x | 1,028 MB | 974 MB |
| Points, clustered as needed | 89.6 s | 6.37 s | 14.1x | 1,165 MB | 1,000 MB |
| Points, all zooms, clustered and summed | 133.6 s | 13.9 s | 9.6x | 1,498 MB | 1,152 MB |
| Roads at z14 | 59.0 s | 1.51 s | 39.1x | 240 MB | 236 MB |
| Tracts at z12 | 196.8 s | 7.77 s | 25.3x | 456 MB | 360 MB |
| Tracts, coalesced as needed | 36.1 s | 4.46 s | 8.1x | 451 MB | 379 MB |

On this machine tippecanoe uses 8 of the 12 cores, since it rounds its thread count down to a power of 2, which keeps its memory low, while manby uses all 12. tippecanoe is slow on the runs with many tiles here because it writes PMTiles by way of a SQLite database next to the output, on a btrfs filesystem that compresses with zstd. With the output in memory instead, roads take it 27 s rather than 59. The kernel also backs memory with 2 MB pages wherever it can (transparent huge pages set to `always`, as Arch sets them), which adds about a tenth to manby's figures, and leaves its roads only just below tippecanoe's.

## Larger than memory

With `--memory-budget=1G`, on inputs several times larger, once each. [Inputs larger than memory](../../guides/memory/) explains the budget.

| Input | System | tippecanoe | manby | tippecanoe memory | manby memory |
| --- | --- | --: | --: | --: | --: |
| 40 million points (5.6 GB) | macOS | 652 s | 79 s | 10,206 MB | 1,139 MB |
| | Linux | 1,025 s | 121 s | 6,379 MB | 1,188 MB |
| The tracts 20 times over (3 GB) | macOS | 564 s | 137 s | 4,810 MB | 1,022 MB |
| | Linux | 854 s | 269 s | 3,730 MB | 1,206 MB |

## Joining

[Joining tilesets](../../guides/join/) has `manby join` against tile-join.

## The commands

Each command runs as `tippecanoe -f -o out.pmtiles` or `manby -f -o out.pmtiles`, followed by these options.

| Benchmark | Options and input |
| --- | --- |
| Points | `-zg --drop-densest-as-needed -P points4m.geojsonl` |
| Points, clustered by distance | `-zg -r1 --cluster-distance=10 --cluster-densest-as-needed --accumulate-attribute=value:sum -P points4m.geojsonl` |
| Points, clustered as needed | `-zg --cluster-densest-as-needed -P points4m.geojsonl` |
| Points, all zooms, clustered and summed | `-zg -r1 --cluster-densest-as-needed --accumulate-attribute=value:sum -P points4m.geojsonl` |
| Roads at z14 | `-z14 -Z0 --drop-densest-as-needed -P tl_2024_us_primaryroads.geojsonl` |
| Tracts at z12 | `-z12 -Z0 --coalesce-densest-as-needed -P cb_2023_us_tract_500k.geojsonl` |
| Tracts, coalesced as needed | `-zg --coalesce-densest-as-needed -P cb_2023_us_tract_500k.geojsonl` |
| 40 million points | `--memory-budget=1G -zg --drop-densest-as-needed -P points40m.geojsonl` (budget for manby only) |
| The tracts 20 times over | `--memory-budget=1G -zg --coalesce-densest-as-needed -P tracts20.geojsonl` (budget for manby only) |

The points are 4 million clustered random points (556 MB), or 40 million (5.6 GB), from `bench/points.py`. The roads are the 17,522 US primary roads of TIGER/Line 2024, which make 122,725 tiles at z14. The tracts are the 85,186 census tracts of the Census cartographic boundaries 2023, which make 312,073 tiles at z12, and 20 times over, moved north and south by `bench/repeat.py`.

## The same tiles

For every run the tiles decode to exactly what tippecanoe makes, with one exception, which comes from tippecanoe's own builds. [Differences from tippecanoe](../differences/) explains it.
