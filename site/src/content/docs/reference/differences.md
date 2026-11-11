---
title: Differences from tippecanoe
description: What is not the same as tippecanoe and tile-join, and why.
---

manby makes tippecanoe's tiles, and `manby join` makes tile-join's. A few things around the tiles differ.

## Archive bytes

The tiles decode to exactly tippecanoe's, but the archives are not tippecanoe's byte for byte, and a little smaller. zlib's exact output decides which features a tile keeps only when the tile could be near the size limit, and every other tile is compressed with libdeflate, which is faster and compresses better. `--compression=zlib` compresses every tile with zlib, as tippecanoe does, for the same archive bytes.

## tippecanoe's arm64 builds

tippecanoe's arm64 builds, from Apple's clang on macOS and from GCC on Linux, fuse multiplications and additions where its x86-64 build does not, and that changes a few tiles near the size limit. On the benchmarks it changes only the census tracts at `-zg`, whose zoom 0 tile comes out 6 bytes smaller compressed, so that tippecanoe's retry keeps other features.

manby makes the same tiles on macOS, Linux and Windows, the tiles of tippecanoe's x86-64 build. Built for x86-64, or on arm64 with `-ffp-contract=off`, tippecanoe makes manby's tiles on the tracts too.

## What tippecanoe does not have

[manby's own options](../options/) lists every option that tippecanoe and tile-join do not have.

- **`--largest-maximum-zoom-guess`.** It chooses the maximum zoom as `-zg` does, but no higher than the zoom it is given, as `--smallest-maximum-zoom-guess` chooses no lower.
- **Filters.** Feature filters also take [`like`, `ilike` and `intersects`](../../guides/programs/#feature-filters).
- **Zooms from attributes.** [`--use-attribute-for-minzoom` and `--use-attribute-for-maxzoom`](../../guides/python/#zooms-from-a-column) take each feature's zooms from its attributes, for inputs without GeoJSON's `tippecanoe` object.

## manby join

- **Shared tiles.** Where several inputs have a tile at the same z/x/y, tile-join orders their features by the tiles' stored bytes. manby join compares tiles that manby made by the bytes zlib would make of them, so manby and then manby join make the tiles that tippecanoe and then tile-join make. tile-join itself, run on manby's tilesets, can order the features of those tiles differently, unless they were made with `--compression=zlib`.
- **Messages.** The progress line names the first tile of each batch, and tiles over the size limit are reported as threads finish them, so in another order.
- **Duplicate tiles.** An MBTiles database without a unique index can have two rows for one z/x/y. tile-join can write such a tile twice, and manby join joins the rows into one tile.
