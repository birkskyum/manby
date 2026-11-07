---
title: Viewing tilesets
description: Serving tilesets with a map of them while they are made, from the command line, R or Python.
---

`manby serve` serves PMTiles archives, MBTiles databases and directories of tiles, with a map of them.

```sh
manby serve --open roads.pmtiles places.mbtiles
```

The map, at `http://127.0.0.1:8080/`, shows each layer of a tileset on a basemap, and the attributes of the features clicked. A tileset made again while it is served shows when the map reloads, so `manby -f` and the map make a quick loop. The map loads MapLibre GL JS and the basemap from the internet.

- **`--port`** is 8080 unless given, or 0 for any free port.
- **`--host`** is 127.0.0.1, so only this computer can reach the server. `--host=0.0.0.0` serves the local network too.
- **`--open`** opens the map in a browser.

It is a server for looking at tilesets, not for hosting them. A PMTiles archive on a bucket or a CDN needs no server.

## In a map of one's own

Each tileset's TileJSON is at `/{name}.json`, its tiles at `/{name}/{z}/{x}/{y}.mvt` (`.mlt` for [MapLibre Tiles](../mlt/)), and a PMTiles archive is also served whole, at its file name, with range requests. Every response allows any origin.

```js
map.addSource('roads', { type: 'vector', url: 'http://127.0.0.1:8080/roads.json' });
```

## From R

`serve()` shows the map in the viewer pane, or a browser, and gives the addresses.

```r
server <- manby::serve("roads.pmtiles")
server$tilejson[["roads"]]
server$stop()
```

With mapgl, the archive goes in as PMTiles.

```r
mapgl::maplibre() |>
  mapgl::add_pmtiles_source(id = "roads", url = "http://127.0.0.1:8080/roads.pmtiles")
```

## From Python

`manby.serve()` opens the map in a browser, or in a notebook shows it there.

```python
server = manby.serve("roads.pmtiles")
server.tilejson["roads"]
server.stop()
```
