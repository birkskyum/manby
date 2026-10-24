---
title: MapLibre Tiles
description: Writing MapLibre Tiles (MLT) instead of Mapbox Vector Tiles, with the same features.
---

`--tile-format=mlt` writes [MapLibre Tiles](https://maplibre.org/maplibre-tile-spec/) instead of Mapbox Vector Tiles, with the same features, into PMTiles, MBTiles or a directory. The tiles are 2 to 45% smaller than MVT on the benchmarks, and MapLibre GL JS 5.12 and later reads them from a vector source with `encoding: "mlt"`.

The encoder, mlt-core, needs Rust 1.98, so MLT output is a cargo feature.

```sh
cargo +1.98.1 build --release --features mlt
manby -zg --drop-densest-as-needed --tile-format=mlt -o places.pmtiles places.geojson
```

`make test-golden-mlt` checks the tiles against the MVT tiles of the same runs.
