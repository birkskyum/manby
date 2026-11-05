---
title: Other formats
description: Tiling GeoPackage, GeoParquet, Shapefile and OpenStreetMap data, converted to FlatGeobuf with GDAL's ogr2ogr.
---

manby reads what tippecanoe reads, GeoJSON, FlatGeobuf, CSV and Geobuf. Convert other formats to FlatGeobuf first, which manby reads in parallel. The examples use GDAL's `ogr2ogr`, and other converters that write FlatGeobuf, such as DuckDB's spatial extension or terrio, work the same way.

## Shapefile

```sh
ogr2ogr -nlt PROMOTE_TO_MULTI -t_srs EPSG:4326 \
  tracts.fgb cb_2023_us_tract_500k.shp
manby -zg --coalesce-densest-as-needed -o tracts.pmtiles tracts.fgb
```

A FlatGeobuf layer has one geometry type, and `-nlt PROMOTE_TO_MULTI` makes mixed polygons and multipolygons, or lines and multilines, all multi.

## GeoPackage

A GeoPackage holds several layers. Convert each one, and name it in the tileset with `-L`.

```sh
ogr2ogr -nlt PROMOTE_TO_MULTI roads.fgb natural_earth_vector.gpkg ne_10m_roads
ogr2ogr -nlt PROMOTE_TO_MULTI places.fgb \
  natural_earth_vector.gpkg ne_10m_populated_places
manby -zg --drop-densest-as-needed -o natural-earth.pmtiles \
  -L roads:roads.fgb -L places:places.fgb
```

## OpenStreetMap

GDAL reads an extract as the layers `points`, `lines`, `multilinestrings`, `multipolygons` and `other_relations`, with the common tags as attributes and the rest in `other_tags`.

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

`ogr2ogr` can write newline-delimited GeoJSON to its standard output, which manby reads in parallel with `-P`. It is a little slower, as GDAL writes GeoJSON more slowly than FlatGeobuf, 5.9 s against 5.0 s on the census tracts.

```sh
ogr2ogr -f GeoJSONSeq /vsistdout/ natural_earth_vector.gpkg ne_10m_roads |
  manby -zg --drop-densest-as-needed -P -l roads -o roads.pmtiles
```

## Coordinates

manby, like tippecanoe, reads longitude and latitude in WGS 84. `-t_srs EPSG:4326` reprojects to it, and GDAL writes newline-delimited GeoJSON in WGS 84 anyway.

## Speed

Converting with `ogr2ogr`, then tiling, once each on an Apple M5 Max with GDAL 3.13 and tippecanoe 2.82.0.

| Input | ogr2ogr, then manby | ogr2ogr, then tippecanoe |
| --- | --: | --: |
| Census tracts, Shapefile (82 MB) | 2.48 + 2.49 = 4.97 s | 2.48 + 24.2 = 26.7 s |
| Natural Earth, GeoPackage, 3 layers | 0.90 + 0.95 = 1.85 s | 0.90 + 5.75 = 6.65 s |
| Liechtenstein, OpenStreetMap, 3 layers | 0.75 + 0.37 = 1.12 s | 0.75 + 4.28 = 5.03 s |
| Nova Scotia water, GeoParquet (226 MB) | 9.31 + 23.9 = 33.2 s | 9.31 + 185 = 194 s |

- manby's tiles decode to exactly tippecanoe's from the same FlatGeobuf file.
- With manby, the conversion is half the time or more on the three smaller inputs.
- The water polygons take longest, as a few tiles at zoom levels 4 to 9 stay over the size limit after coalescing, and each retry cleans their large polygons again.
- The tracts are the 85,186 of the Census cartographic boundaries 2023, and the water the 169,852 polygons of the Nova Scotia hydrographic network.

## Feature order

`ogr2ogr` gives FlatGeobuf a spatial index by default, which stores the features in spatial order, and tippecanoe's choices of what to drop or coalesce can follow the order. So the tiles can differ slightly from those of the input's order. `-lco SPATIAL_INDEX=NO` keeps it.
