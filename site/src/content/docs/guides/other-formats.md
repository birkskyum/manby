---
title: Other formats, with terrio
description: Tiling GeoPackage, GeoParquet, Shapefile and OpenStreetMap data, converted to FlatGeobuf with terrio.
---

manby reads what tippecanoe reads, GeoJSON, FlatGeobuf, CSV and Geobuf. For other formats, [terrio](https://terrio.opengeos.org/), a converter in pure Rust with GDAL's options, writes FlatGeobuf, which manby reads in parallel. terrio is a single program with no libraries to install, and its [installation page](https://terrio.opengeos.org/installation/) has the ways to get it.

## Shapefile

```sh
terrio convert --t-srs EPSG:4326 cb_2023_us_tract_500k.shp tracts.fgb
manby -zg --coalesce-densest-as-needed -o tracts.pmtiles tracts.fgb
```

## GeoPackage

A GeoPackage holds several layers. `-l` picks one, and manby's `-L` names each file's layer in the tileset.

```sh
terrio convert -l ne_10m_roads natural_earth_vector.gpkg roads.fgb
terrio convert -l ne_10m_populated_places natural_earth_vector.gpkg places.fgb
manby -zg --drop-densest-as-needed -o natural-earth.pmtiles \
  -L roads:roads.fgb -L places:places.fgb
```

## OpenStreetMap

terrio reads an OpenStreetMap extract into the layers GDAL makes of it, such as `points`, `lines` and `multipolygons`, with the common tags as attributes and the rest in `other_tags`.

```sh
terrio convert -l points liechtenstein-latest.osm.pbf points.fgb
terrio convert -l lines liechtenstein-latest.osm.pbf lines.fgb
terrio convert -l multipolygons liechtenstein-latest.osm.pbf areas.fgb
manby -z14 --drop-densest-as-needed -o liechtenstein.pmtiles \
  -L points:points.fgb -L lines:lines.fgb -L areas:areas.fgb
```

## GeoParquet

```sh
terrio convert --s-srs EPSG:2961 --t-srs EPSG:4326 \
  ns-water_water-poly_geo.parquet water.fgb
manby -zg --coalesce-densest-as-needed -o water.pmtiles water.fgb
```

## Coordinates

manby, like tippecanoe, reads coordinates as longitude and latitude in WGS 84, and `--t-srs EPSG:4326` reprojects to it. When terrio cannot read a file's coordinate system, `terrio info` shows `CRS: unknown`, and `--s-srs` names it. Nova Scotia's water polygons declare a compound coordinate system that terrio 0.5.0 does not read, so the command above names the UTM zone they are in.

## Speed

Once each, on an Apple M5 Max, with terrio 0.5.0 and the commands above. The census tracts are the 85,186 of the Census cartographic boundaries 2023, and the water the 169,852 polygons of the Nova Scotia hydrographic network. The tippecanoe column is tippecanoe 2.82.0 on the same FlatGeobuf files, and for every input manby's tiles decode to exactly tippecanoe's.

| Input | terrio | manby | tippecanoe |
| --- | --: | --: | --: |
| Census tracts, Shapefile (82 MB) | 0.24 s | 2.24 s | 23.5 s |
| Natural Earth, GeoPackage, 3 layers | 0.17 s | 0.96 s | 5.53 s |
| Liechtenstein, OpenStreetMap, 3 layers | 0.18 s | 0.33 s | 4.60 s |
| Nova Scotia water, GeoParquet (226 MB) | 2.07 s | 63.7 s | 180 s |

The water polygons are slower than the rest because a few of their tiles at zoom levels 4 to 9 are over the size limit even after coalescing, and each retry cleans their large polygons again.

## Compared with GDAL's conversion

terrio's FlatGeobuf files hold the features GDAL's hold, with two differences that change a few tiles. Features at the same place in the spatial index can come in another order, and on the census tracts terrio reads 4 of the 85,186 polygons with holes as two separate polygons, with the hole filled in. The tiles of a FlatGeobuf file can also differ slightly from those of the same data as GeoJSON, since a FlatGeobuf with a spatial index, terrio's default, stores its features in spatial order rather than the input's.
