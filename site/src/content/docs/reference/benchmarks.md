---
title: Benchmarks
description: manby against tippecanoe 2.82.0 on macOS and on Linux, with the same tiles, and against tylertoo, in time and peak memory.
---

Against tippecanoe 2.82.0 itself, the C++ built with its own Makefile, writing PMTiles. Each figure is the median of three runs, and memory is peak resident memory. tippecanoe's runs and manby's come from separate sessions on the same machine, since tippecanoe's do not change with manby's version. The points are 4 million random points (556 MB of GeoJSON), the roads the 17,522 US primary roads of TIGER/Line 2024, and the tracts the 85,186 US census tracts of the Census cartographic boundaries 2023. The [commands](#the-commands) are below, and the scripts are in [`bench/`](https://github.com/birkskyum/manby/tree/main/bench).

## macOS

An Apple M5 Max with 18 cores and 128 GB.

### Time

| Benchmark | tippecanoe | manby | Speedup |
| --- | --: | --: | --: |
| Points, dropping the densest as needed | 61.4 s | 3.44 s | 17.8x |
| Points, clustered within 10 pixels, values summed | 54.4 s | 3.88 s | 14.0x |
| Points, clustering the densest as needed | 59.8 s | 4.07 s | 14.7x |
| Points, clustered at every zoom, values summed | 87.7 s | 7.82 s | 11.2x |
| Roads to z14 | 11.8 s | 0.81 s | 14.5x |
| Tracts to z12 | 46.6 s | 3.78 s | 12.3x |
| Tracts, coalescing the densest as needed | 24.8 s | 2.27 s | 10.9x |

### Peak memory

| Benchmark | tippecanoe | manby |
| --- | --: | --: |
| Points, dropping the densest as needed | 2,276 MB | 919 MB |
| Points, clustered within 10 pixels, values summed | 1,318 MB | 891 MB |
| Points, clustering the densest as needed | 2,330 MB | 916 MB |
| Points, clustered at every zoom, values summed | 3,443 MB | 1,115 MB |
| Roads to z14 | 327 MB | 177 MB |
| Tracts to z12 | 566 MB | 338 MB |
| Tracts, coalescing the densest as needed | 554 MB | 366 MB |

## Linux

A Lenovo Yoga Slim 7x with a Snapdragon X Elite (12 cores, 32 GB), running Arch Linux ARM.

### Time

| Benchmark | tippecanoe | manby | Speedup |
| --- | --: | --: | --: |
| Points, dropping the densest as needed | 88.5 s | 5.86 s | 15.1x |
| Points, clustered within 10 pixels, values summed | 82.5 s | 6.02 s | 13.7x |
| Points, clustering the densest as needed | 89.6 s | 6.37 s | 14.1x |
| Points, clustered at every zoom, values summed | 133.6 s | 13.9 s | 9.6x |
| Roads to z14 | 59.0 s | 1.51 s | 39.1x |
| Tracts to z12 | 196.8 s | 7.77 s | 25.3x |
| Tracts, coalescing the densest as needed | 36.1 s | 4.46 s | 8.1x |

### Peak memory

| Benchmark | tippecanoe | manby |
| --- | --: | --: |
| Points, dropping the densest as needed | 1,088 MB | 994 MB |
| Points, clustered within 10 pixels, values summed | 1,028 MB | 974 MB |
| Points, clustering the densest as needed | 1,165 MB | 1,000 MB |
| Points, clustered at every zoom, values summed | 1,498 MB | 1,152 MB |
| Roads to z14 | 240 MB | 236 MB |
| Tracts to z12 | 456 MB | 360 MB |
| Tracts, coalescing the densest as needed | 451 MB | 379 MB |

On this machine tippecanoe uses 8 of the 12 cores, since it rounds its thread count down to a power of 2, which keeps its memory low, while manby uses all 12. tippecanoe is slow on the runs with many tiles here because it writes PMTiles by way of a SQLite database next to the output, on a btrfs filesystem that compresses with zstd. With the output in memory instead, roads take it 27 s rather than 59. The kernel also backs memory with 2 MB pages wherever it can (transparent huge pages set to `always`, as Arch sets them), which adds about a tenth to manby's figures, and leaves its roads only just below tippecanoe's.

## Larger than memory

With `--memory-budget=1G`, on inputs several times larger, once each. [Inputs larger than memory](../../guides/memory/) explains the budget.

| Input | System | tippecanoe | manby |
| --- | --- | --: | --: |
| 40 million points (5.6 GB) | macOS | 652 s, 10,206 MB | 79 s, 1,139 MB |
| | Linux | 1,025 s, 6,379 MB | 121 s, 1,188 MB |
| The tracts 20 times over (3 GB) | macOS | 564 s, 4,810 MB | 137 s, 1,022 MB |
| | Linux | 854 s, 3,730 MB | 269 s, 1,206 MB |

## Against tylertoo

[tylertoo](https://github.com/geoparquet-io/tylertoo) tiles GeoParquet, and makes tiles of its own rather than tippecanoe's ([Why manby?](../../why/#tylertoo) has how), so this compares the time each takes on the same features, not the same tiles. tylertoo read the features as GeoParquet and manby as GeoJSON. Both made the same zoom levels, those manby's `-zg` chose, with a 500 kB tile size limit, and tylertoo used its defaults otherwise, from its main branch (6c3cec4), since its release 0.8.0 does not have the size options yet. Each figure is the median of three runs, alternating, on the Mac above, in one session, so manby's figures differ a little from the tables above.

| Benchmark | tylertoo | manby |
| --- | --: | --: |
| Points to z6, dropping the densest as needed | 5.61 s, 1,329 MB | 3.98 s, 913 MB |
| Roads to z14, dropping the densest as needed | 1.85 s, 1,447 MB | 0.89 s, 180 MB |
| Tracts to z12 | 3.39 s, 1,420 MB | 4.07 s, 342 MB |
| Tracts to z7, coalescing the densest as needed | 0.90 s, 1,139 MB | 2.69 s, 377 MB |

tylertoo is faster on the tracts, three times at z7 and 1.2 times at z12. It fits each tile to the size limit on its own, where tippecanoe, and so manby, raises one threshold for a whole zoom level and makes the zoom level again until every tile fits, and that is most of manby's time on the tracts at low zoom levels. Its tiles hold more, 61,746 tract polygons at z5 where tippecanoe's hold 17,094, and at its defaults it thins points more, 1,094 at z0 where tippecanoe keeps 16,384. Its size limit is also 512,000 bytes, where tippecanoe's is 500,000. manby took the same time from FlatGeobuf as from GeoJSON.

tylertoo ran as `tylertoo tiles` with `--max-zoom 6 --size-valve drop` for the points, `--max-zoom 14 --size-valve drop` for the roads, and `--max-zoom 12` or `--max-zoom 7` for the tracts, whose default size valve merges the densest polygons. DuckDB made the GeoParquet files from the GeoJSON.

## Joining

[Joining tilesets](../../guides/join/) has `manby join` against tile-join.

## The commands

Each command runs as `tippecanoe -f -o out.pmtiles` or `manby -f -o out.pmtiles`, followed by these options.

| Benchmark | Options and input |
| --- | --- |
| Points, dropping the densest as needed | `-zg --drop-densest-as-needed -P points4m.geojsonl` |
| Points, clustered within 10 pixels, values summed | `-zg -r1 --cluster-distance=10 --cluster-densest-as-needed --accumulate-attribute=value:sum -P points4m.geojsonl` |
| Points, clustering the densest as needed | `-zg --cluster-densest-as-needed -P points4m.geojsonl` |
| Points, clustered at every zoom, values summed | `-zg -r1 --cluster-densest-as-needed --accumulate-attribute=value:sum -P points4m.geojsonl` |
| Roads to z14 | `-z14 -Z0 --drop-densest-as-needed -P tl_2024_us_primaryroads.geojsonl` |
| Tracts to z12 | `-z12 -Z0 --coalesce-densest-as-needed -P cb_2023_us_tract_500k.geojsonl` |
| Tracts, coalescing the densest as needed | `-zg --coalesce-densest-as-needed -P cb_2023_us_tract_500k.geojsonl` |
| 40 million points | `--memory-budget=1G -zg --drop-densest-as-needed -P points40m.geojsonl` (budget for manby only) |
| The tracts 20 times over | `--memory-budget=1G -zg --coalesce-densest-as-needed -P tracts20.geojsonl` (budget for manby only) |

The points are 4 million clustered random points (556 MB), or 40 million (5.6 GB), from `bench/points.py`. The roads are the 17,522 US primary roads of TIGER/Line 2024, which make 122,725 tiles at z14. The tracts are the 85,186 census tracts of the Census cartographic boundaries 2023, which make 312,073 tiles at z12, and 20 times over, moved north and south by `bench/repeat.py`.

## The same tiles

For every run the tiles decode to exactly what tippecanoe makes, with one exception, which comes from tippecanoe's own builds. [Differences from tippecanoe](../differences/) explains it.
