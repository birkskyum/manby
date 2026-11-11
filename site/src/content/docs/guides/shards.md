---
title: Builds split across machines
description: --shard I/N makes part of a tileset, and manby merge puts the parts together as the whole run makes them.
---

`--shard I/N` makes part `I` of `N` of a tileset, so the parts of one build can run on different machines at once. `manby merge` puts them together. The merged tileset is the one the run without `--shard` makes, byte for byte.

```sh
# on each of four machines, with the same input and options
manby -z12 --drop-densest-as-needed --shard 0/4 -o part-0.pmtiles areas.geojsonl
manby -z12 --drop-densest-as-needed --shard 1/4 -o part-1.pmtiles areas.geojsonl
manby -z12 --drop-densest-as-needed --shard 2/4 -o part-2.pmtiles areas.geojsonl
manby -z12 --drop-densest-as-needed --shard 3/4 -o part-3.pmtiles areas.geojsonl

# then, wherever the parts are
manby merge -o areas.pmtiles part-0.pmtiles part-1.pmtiles part-2.pmtiles part-3.pmtiles
```

## How the work is split

Every shard reads all of the input, so that every choice made from the whole input comes out the same, such as the zoom that `-zg` guesses or the zoom each feature first shows at. Each shard needs as much memory as the whole run, or `--memory-budget`.

The tiles from the pivot zoom down are split by their tile at the pivot, in runs that each hold about as much work, and each shard makes the tiles in its run. Shard 0 also makes the zooms above the pivot. The pivot is zoom 6, kept between the minimum and the maximum zoom. `--shard-zoom` sets another.

On Canada's 2021 dissemination areas (754 MB of GeoJSON) at `-z12 --drop-densest-as-needed`, on an Apple M5 Max:

| Run | Time |
| --- | --- |
| Whole | 11.7 to 12.3 s |
| 4 shards | 3.8 to 5.2 s each |
| 8 shards | 1.7 to 3.9 s each |
| Merge | 0.4 s |

Each shard ran with the whole machine, one after the other. With many shards, shard 0 takes about the longest, as it also makes zooms 0 to 5.

## Where the parts can differ from the whole run

When a tile is too big, the `-as-needed` options make its whole zoom level again with a stricter threshold. At the pivot zoom and below, a shard sees only its own tiles, so it makes such a level again for its tiles alone, and the other shards keep the threshold they had. `--extend-zooms-if-still-dropping` is the same. The shard notes the zooms where this happened, and `manby merge` lists them as zooms that can differ from the whole run's.

With [`--as-needed-per-tile`](../per-tile/), each tile keeps its own threshold, so the shards make the whole run's tiles at every zoom. The zooms above the pivot are always the whole run's, since shard 0 makes all of them.

## The parts

- Every shard takes the same input and options, apart from `--shard` and `-o`. `manby merge` checks that the shards are of one run, one of each, and that each holds only its own tiles.
- A shard is a tileset of its own (PMTiles, MBTiles or a directory) with its part of the tiles. Its metadata holds what the merge needs, under `manby_shard`.
- `manby merge` writes PMTiles or MBTiles with `-o`, or a directory with `-e`. The merged tileset is named after its path, as a whole run's is, unless the shards had `-n`. Its `generator_options` are shard 0's command line without `--shard`.
- `--generate-variable-depth-tile-pyramid` does not work with `--shard`.
