#!/bin/bash
# Downloads and converts the benchmark data into bench/data (about 1 GB).
# Needs curl and ogr2ogr (GDAL).
set -e
cd "$(dirname "$0")"
mkdir -p data/download
for z in GENZ2023/shp/cb_2023_us_tract_500k TIGER2024/PRIMARYROADS/tl_2024_us_primaryroads TIGER2024/COUNTY/tl_2024_us_county; do
	name=$(basename $z)
	[ -f data/$name.geojsonl ] && continue
	curl -sSfL -o data/download/$name.zip https://www2.census.gov/geo/tiger/$z.zip
	ogr2ogr -f GeoJSONSeq -t_srs EPSG:4326 data/$name.geojsonl /vsizip/data/download/$name.zip/$name.shp
done
[ -f data/points4m.geojsonl ] || python3 -I points.py 4000000 > data/points4m.geojsonl
ls -la data
