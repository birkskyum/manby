---
title: Inputs larger than memory
description: How --memory-budget keeps manby's memory near a budget, for inputs larger than the machine's memory.
---

manby keeps its features in memory, which is much of why it is fast, and its peak memory is still below tippecanoe's on every benchmark. For inputs larger than memory, `--memory-budget` bounds it. The default is half the physical memory, as tippecanoe sizes its sorting.

```sh
manby --memory-budget=1G -zg --drop-densest-as-needed -o points.pmtiles points40m.geojsonl
```

Past the budget, the features and the tile data live in an unlinked temporary file in the `-t` directory, mapped into memory and laid out in the order the tiling reads them, and manby keeps its resident memory near the budget. Under a budget, the tiles being made take most of what is resident, so the biggest tiles run together only as far as three quarters of the budget allows.

Inputs several times larger than a 1 GB budget, once each, against tippecanoe 2.82.0:

| Input | System | tippecanoe | manby |
| --- | --- | --: | --: |
| 40 million points (5.6 GB) | macOS | 652 s, 10,206 MB | 79 s, 1,139 MB |
| | Linux | 1,025 s, 6,379 MB | 121 s, 1,188 MB |
| The census tracts 20 times over (3 GB) | macOS | 564 s, 4,810 MB | 137 s, 1,022 MB |
| | Linux | 854 s, 3,730 MB | 269 s, 1,206 MB |

The tiles are tippecanoe's for both. The [benchmarks](../../reference/benchmarks/) give the commands and describe the machines.
