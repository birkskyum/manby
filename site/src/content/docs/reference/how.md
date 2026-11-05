---
title: How it works
description: Where tippecanoe spends its time, and how manby makes the same tiles without spending it.
---

tippecanoe spends most of its time waiting on itself rather than computing. It works through the low zoom levels one tile per thread, rebuilds oversized tiles by reading their features back from compressed temporary files, finishes each zoom level before starting the next, and writes PMTiles by way of a SQLite database.

manby keeps the features in memory, sorted once, walks the tile pyramid in parallel on every core, splits big tiles across threads, retries oversized tiles on data already in memory, and writes PMTiles directly. The geometry algorithms are tippecanoe's own, by way of [tippecanoe-rs](https://github.com/birkskyum/manby/tree/main/crates/tippecanoe-rs), which is why the tiles come out the same.

## Where tippecanoe spends its time

Measured from tippecanoe's own progress output.

- **Low zooms on few cores.** For the 4 million points, zoom levels 0 to 2 take 39 of the 57 seconds. Those levels hold 21 tiles, and tippecanoe works on each tile with a single thread.
- **Size limit retries.** Tiles over 500 KB are rebuilt with more features dropped, about 85 times for the points and 5 times for the tracts' single zoom 0 tile. Each retry reads all of the tile's features back from a compressed temporary file.
- **Idle cores.** Averaged over a run, tippecanoe keeps 2 to 3 of the 18 cores busy.
- **Zoom by zoom.** Every zoom level finishes before the next starts, and all features pass through zlib-compressed temporary files once per zoom.
- **PMTiles by way of SQLite.** PMTiles output is written as a complete MBTiles database first, checked, copied into a temporary file, and copied once more into the archive. That adds 22 to 31% on runs with hundreds of thousands of tiles.

## The pipeline

1. **Reading.** The input is split into byte ranges at line or feature boundaries, and each thread parses its range into its own part of the store, with its own interned keys and values. The parts are then joined and the ids remapped.
2. **The store.** All features are fixed-size records, with their geometry and attributes in shared arrays, and keys and values interned once.
3. **Ordering.** The records are sorted by tippecanoe's order with a parallel radix sort, then one ordered pass assigns each feature its minimum zoom, as tippecanoe does.
4. **Tiling.** The tiler walks the tile quadtree from the top. Each tile is a task whose input is the features its parent passed down, and children start as soon as their parent is read, so all cores stay busy without waiting for whole zoom levels. A tile with many features splits its work across threads. A tile over the size or feature limit retries only the selection and encoding of data already in memory, choosing the next threshold exactly as tippecanoe does.
5. **Writing.** PMTiles archives are written directly, with tiles deduplicated by content and laid out in tile order. MBTiles go through one writer thread in large transactions, and directories are written in parallel.

## Compression

The size limit is checked on the compressed size, so the size zlib makes at tippecanoe's settings decides which features a tile keeps. manby runs zlib only where that size can change a decision. When even zlib's bound on its output fits the limit, the tile is kept whatever zlib makes of it, so manby compresses it with libdeflate instead, which is faster and makes smaller tiles. A tile near or over the limit goes through zlib, and when it is kept, that zlib stream is stored as it is.

So the tiles decode to exactly tippecanoe's, and the archives are a little smaller, but not byte for byte tippecanoe's. `--compression=zlib` stores every tile as zlib makes it, for tippecanoe's archive bytes.

## Memory

manby holds a tile's features while it makes the tile, so it limits how many big tiles are made at once by an estimate of their memory, and drops each zoom level's data as soon as the level is written. [Inputs larger than memory](../../guides/memory/) describes the budget for inputs that do not fit.

The full design, with its measurements, is in [DESIGN.md](https://github.com/birkskyum/manby/blob/main/DESIGN.md).
