---
title: tippecanoe's other programs
description: manby also runs tippecanoe-decode, tippecanoe-enumerate, tippecanoe-overzoom and tippecanoe-json-tool, so one program does all of tippecanoe's work.
---

manby also runs tippecanoe's smaller programs, as tippecanoe-rs has them, so one program does all of tippecanoe's work.

| tippecanoe | manby |
| --- | --- |
| `tippecanoe` | `manby` |
| `tile-join` | `manby join` |
| `tippecanoe-decode` | `manby decode` |
| `tippecanoe-enumerate` | `manby enumerate` |
| `tippecanoe-overzoom` | `manby overzoom` |
| `tippecanoe-json-tool` | `manby json-tool` |

Each program's own name works as the subcommand too (`manby tippecanoe-decode`), and under that name, through a link, manby is that program. So links named after all six make manby a drop-in replacement for tippecanoe.

```sh
for p in tippecanoe tile-join tippecanoe-decode tippecanoe-enumerate tippecanoe-overzoom tippecanoe-json-tool; do
  ln -s "$(which manby)" ~/.local/bin/$p
done
```

The tiling and the joining are manby's own, since that is where the time goes, and so is overzooming, for its feature filters. Decoding, enumerating and json-tool are tippecanoe-rs's code and run at its speed.

## Feature filters

Feature filters (`-j`, `-J`) take tippecanoe's expressions and three more.

- `["like", key, pattern]` is SQL's LIKE, with `%` for any run of characters and `_` for any one. A number or boolean is matched as its text.
- `["ilike", key, pattern]` is the same but ignores case. `!like` and `!ilike` are their negations.
- `["intersects", geometry]`, in `manby overzoom`, keeps the features that share a point with a GeoJSON Polygon or MultiPolygon in longitude and latitude, as clipped to the new tile.

```sh
manby -zg -j '{"*":["ilike","name","%park%"]}' -o parks.pmtiles places.geojson
```

To tile an input file named like a subcommand, such as `decode`, write it as `./decode`.
