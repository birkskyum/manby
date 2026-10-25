---
title: Other formats
description: Tiling GeoPackage, GeoParquet, Shapefile and OpenStreetMap data, converted to FlatGeobuf with GDAL's ogr2ogr.
---

manby reads what tippecanoe reads, GeoJSON, FlatGeobuf, CSV and Geobuf. Other formats go through a converter first, and FlatGeobuf is the best one to convert to, since manby reads it in parallel. The examples use GDAL's `ogr2ogr`, which reads almost every vector format. Other converters that write FlatGeobuf or newline-delimited GeoJSON, such as DuckDB's spatial extension or terrio, work the same way.

## Shapefile

```sh
ogr2ogr -nlt PROMOTE_TO_MULTI -t_srs EPSG:4326 \
  tracts.fgb cb_2023_us_tract_500k.shp
manby -zg --coalesce-densest-as-needed -o tracts.pmtiles tracts.fgb
```

A FlatGeobuf layer has one geometry type, and Shapefiles and GeoPackages often mix polygons and multipolygons, or lines and multilines. `-nlt PROMOTE_TO_MULTI` makes them all multi.

## GeoPackage

A GeoPackage holds several layers. `ogr2ogr` converts the one it is given, and manby's `-L` names each file's layer in the tileset.

```sh
ogr2ogr -nlt PROMOTE_TO_MULTI roads.fgb natural_earth_vector.gpkg ne_10m_roads
ogr2ogr -nlt PROMOTE_TO_MULTI places.fgb \
  natural_earth_vector.gpkg ne_10m_populated_places
manby -zg --drop-densest-as-needed -o natural-earth.pmtiles \
  -L roads:roads.fgb -L places:places.fgb
```

## OpenStreetMap

GDAL reads an OpenStreetMap extract into the layers `points`, `lines`, `multilinestrings`, `multipolygons` and `other_relations`, with the common tags as attributes and the rest in `other_tags`.

```sh
ogr2ogr points.fgb liechtenstein-latest.osm.pbf points
ogr2ogr lines.fgb liechtenstein-latest.osm.pbf lines
ogr2ogr areas.fgb liechtenstein-latest.osm.pbf multipolygons
manby -z14 --drop-densest-as-needed -o liechtenstein.pmtiles \
  -L points:points.fgb -L lines:lines.fgb -L areas:areas.fgb
```

## GeoParquet

```sh
ogr2ogr -t_srs EPSG:4326 water.fgb ns-water_water-poly_geo.parquet
manby -zg --coalesce-densest-as-needed -o water.pmtiles water.fgb
```

## Without a file in between

`ogr2ogr` can write newline-delimited GeoJSON to its standard output, and manby reads its standard input, so a pipe skips the FlatGeobuf file. `-l` names the layer, and `-P` reads it in parallel, from a pipe as from a file. GDAL writes GeoJSON more slowly than FlatGeobuf, though, so on the census tracts the pipe took 5.9 s, and the two steps through a FlatGeobuf file 5.0 s.

```sh
ogr2ogr -f GeoJSONSeq /vsistdout/ natural_earth_vector.gpkg ne_10m_roads |
  manby -zg --drop-densest-as-needed -P -l roads -o roads.pmtiles
```

## Coordinates

manby, like tippecanoe, reads coordinates as longitude and latitude in WGS 84. `-t_srs EPSG:4326` reprojects to it, and GDAL writes newline-delimited GeoJSON in WGS 84 without being asked.

## Speed

Each run has two steps. `ogr2ogr` converts the input to FlatGeobuf, and then manby, or tippecanoe, tiles that file into PMTiles. The first step is the same for both.

| Input | ogr2ogr, then manby | ogr2ogr, then tippecanoe |
| --- | --: | --: |
| Census tracts, Shapefile (82 MB) | 2.48 + 2.49 = 4.97 s | 2.48 + 24.2 = 26.7 s |
| Natural Earth, GeoPackage, 3 layers | 0.90 + 0.95 = 1.85 s | 0.90 + 5.75 = 6.65 s |
| Liechtenstein, OpenStreetMap, 3 layers | 0.75 + 0.37 = 1.12 s | 0.75 + 4.28 = 5.03 s |
| Nova Scotia water, GeoParquet (226 MB) | 9.31 + 23.9 = 33.2 s | 9.31 + 185 = 194 s |

Once each, on an Apple M5 Max, with GDAL 3.13, tippecanoe 2.82.0 and the commands above. The GeoPackage row adds a third layer, the states and provinces. The census tracts are the 85,186 of the Census cartographic boundaries 2023, and the water the 169,852 polygons of the Nova Scotia hydrographic network. For every input, manby's tiles decode to exactly tippecanoe's from the same FlatGeobuf file. With manby, the conversion is half the time or more on the three smaller inputs, where with tippecanoe it was a small part.

The water polygons take longer than the rest, for manby and for tippecanoe, because a few of their tiles at zoom levels 4 to 9 are over the size limit even after coalescing, and each retry cleans their large polygons again.

## Feature order

A FlatGeobuf file with a spatial index, `ogr2ogr`'s default, stores its features in spatial order rather than the input's, and tippecanoe's choices of which features to drop or coalesce can follow the order. So its tiles can differ slightly from those of the same data in the input's order. `-lco SPATIAL_INDEX=NO` keeps the input's order.
