---
title: Fitting each tile on its own
description: --as-needed-per-tile thins only the tiles that are too big, not their whole zoom level.
---

When a tile is too big, the `-as-needed` options raise a threshold for it, such as how sparse a point must be to stay, until it fits. tippecanoe then makes the whole zoom level again with the highest threshold any of its tiles needed, so every tile at that zoom follows one rule. With `--as-needed-per-tile`, each tile keeps the threshold it needed, and no zoom level is made again.

```sh
manby -zg --coalesce-densest-as-needed --as-needed-per-tile -o tracts.pmtiles tracts.geojsonl
```

- **More detail where there is room.** Only the tiles that are too big are thinned. On the [census tracts](../../reference/benchmarks/), the tiles of z5 keep 67,163 tract polygons rather than 17,094, and the archive is 27 MB rather than 21 MB.
- **Seams at tile edges.** Neighboring tiles can be thinned differently, so the density of points can change at a tile edge, and a line or polygon can be kept in one tile and dropped or merged in the next.
- **A little faster.** 1.1 to 1.2 times on the tracts and the points, as no zoom level is made twice.
- **Not tippecanoe's tiles.** tippecanoe fits each tile on its own only with `--force-feature-limit`, which drops a fraction of the features.
