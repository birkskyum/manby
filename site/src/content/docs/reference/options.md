---
title: manby's own options
description: The options manby has that tippecanoe and tile-join do not, in one place.
---

manby takes [tippecanoe's options](https://github.com/felt/tippecanoe#readme), and `manby join` takes [tile-join's](https://github.com/felt/tippecanoe#tile-join). These are the ones they do not have. `manby --help` lists every option, with manby's under a heading of their own.

## Tiling

| Option | What it does |
| --- | --- |
| `--memory-budget=SIZE` | Keeps memory near `SIZE`, such as `1G` or `500M`, with the rest in a temporary file. Half the physical memory unless given. See [Inputs larger than memory](../../guides/memory/). |
| `--threads=N` | The number of threads, one for each CPU unless given. The tiles are the same with any number. |
| `--shard=I/N` | Makes part `I` of `N` of the tileset, which `manby merge` puts together with the others. See [Builds split across machines](../../guides/shards/). |
| `--shard-zoom=Z` | The zoom whose tiles the shards split, 6 unless given. |
| `--largest-maximum-zoom-guess=Z` | Chooses the maximum zoom as `-zg` does, but no higher than `Z`. |
| `--bbox=MINLON,MINLAT,MAXLON,MAXLAT` | Keeps the rows of GeoParquet and Arrow inputs whose bounding boxes overlap the box. See [GeoParquet, DuckDB and Python](../../guides/python/). |
| `--h3` | Bins each layer's points into H3 hexagons below a base zoom. See [H3 hexagons and categories](../../guides/h3/). |
| `--h3-base-zoom=Z` | The zoom from which the points themselves show, two below the maximum zoom unless given. Implies `--h3`. |
| `--h3-categories=NAME` | Counts the values of an attribute in each hexagon. Implies `--h3`. |
| `--h3-median=NAME` | The median of an attribute's values in each hexagon. Takes a list, separated by commas. Implies `--h3`. |
| `--h3-resolutions=Z:R,...` | The H3 resolution at each zoom given, over the built-in ones. Implies `--h3`. |
| `--h3-fade=N` | Each resolution shows `N` zooms beyond its own on each side, and the points `N` zooms before the base zoom, to fade from one to the next. Implies `--h3`. |
| `--h3-no-points` | Only the hexagons, at every zoom, without the points. Implies `--h3`. |
| `--h3-join=FILE` | Attributes for the hexagons from a Parquet file, by the cell in its `h3` column, and the layer in its `layer` column if it has one. Implies `--h3`. |
| `--supercluster` | Clusters each layer's points with Supercluster, as a MapLibre GeoJSON source does, with its properties and `cluster_expansion_zoom`. See [Clusters as MapLibre makes them](../../guides/supercluster/). |
| `--supercluster-radius=PIXELS` | The radius of a cluster in pixels of a 512-pixel tile, 50 unless given. |
| `--supercluster-min-points=N` | The fewest points that make a cluster, 2 unless given. |
| `--cluster-categories=NAME` | Counts the values of an attribute in each cluster. `NAME:a,b` counts only `a` and `b` by name, and the rest together. |
| `--use-attribute-for-minzoom=NAME` | Takes the lowest zoom each feature shows at from an attribute, as the `tippecanoe` object gives it in GeoJSON. See [Zooms from a column](../../guides/python/#zooms-from-a-column). |
| `--use-attribute-for-maxzoom=NAME` | The same for the highest zoom each feature shows at. |
| `--tile-format=mlt` | Writes [MapLibre Tiles](../../guides/mlt/) instead of Mapbox Vector Tiles. `mvt` is the default. |
| `--mlt-profile=PROFILE` | The encodings MapLibre Tiles may use: `web`, the default, or `native`, `min-size` or `fast`. |
| `--compression=zlib` | Stores every tile as zlib compresses it, for an archive that is byte for byte tippecanoe's. `libdeflate` is the default. See [Differences from tippecanoe](../differences/#archive-bytes). |

Two options make tiles that are not tippecanoe's.

| Option | What it does |
| --- | --- |
| `--as-needed-per-tile` | A tile that is too big keeps the threshold it needed, and its zoom level is not made again with the highest. See [Fitting each tile on its own](../../guides/per-tile/). |
| `--fast` | Checks the size limit on the bytes libdeflate makes of a tile, not zlib's, so no tile goes through zlib. A tile near the limit can keep other features than tippecanoe's. |

## Options that take more

- **`-s`, `--projection`** also takes any of the 9,508 EPSG and ESRI codes of [crs-defs](https://github.com/birkskyum/crs-defs), such as `-s EPSG:28992`.
- **`-j`, `-J`** feature filters also take [`like`, `ilike` and `intersects`](../../guides/programs/#feature-filters).
- **Inputs** can also be GeoParquet and Arrow files, and GeoParquet at `s3://`, `gs://`, `az://` and `https://` URLs.

## manby merge

`manby merge -o out.pmtiles part-0.pmtiles part-1.pmtiles ...` puts the parts made with `--shard` together. `-o` writes PMTiles or MBTiles, `-e` a directory, `-f` replaces an existing tileset, and `-q` leaves out the notes. See [Builds split across machines](../../guides/shards/).

## manby join

| Option | What it does |
| --- | --- |
| `--threads=N` | The number of threads, one for each CPU unless given. |
| `--compression=zlib` | Stores every tile as zlib compresses it, as tile-join does. `libdeflate` is the default. |

[Joining tilesets](../../guides/join/) has the rest.

## manby stats and manby serve

| Command | Options |
| --- | --- |
| `manby stats` | `--layers` counts each layer's features at each zoom, `--largest=N` lists the `N` largest tiles, 10 unless given, and `--json` writes the report as JSON. |
| `manby serve` | `--port=PORT`, 8080 unless given, `--host=HOST`, 127.0.0.1 unless given, and `--open` opens the map in a browser. |

[Looking at tilesets](../../guides/serve/) describes both.

## Environment

- **`TIPPECANOE_MAX_THREADS`** is read as tippecanoe reads it. It decides what tippecanoe's thread count decides in its tiles, such as the ids that `--generate-ids` gives, and `manby join` takes it as its number of threads, as tile-join does. It does not limit manby's threads when tiling, which `--threads` does.
- **`AWS_ACCESS_KEY_ID`, `AWS_REGION`, `GOOGLE_APPLICATION_CREDENTIALS`** and the like sign the requests for GeoParquet in object storage. Without them the requests go unsigned.
- **`MANBY_BINARY`** names the `manby` that the R package runs, in place of the one it downloads.
