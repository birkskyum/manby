"""The Python module: the same features from a GeoParquet file, DuckDB,
GeoPandas, Polars and PyArrow make the same tileset.

Run with the wheel installed: python tests/python/test_tile.py
"""

import gzip
import json
import os
import struct
import subprocess
import sys
import tempfile
import urllib.request

import duckdb
import geopandas
import polars
import pyarrow as pa
import pyarrow.parquet as pq
import shapely

import manby


def tiles(path):
    """A PMTiles archive's tile data and layers."""
    with open(path, "rb") as f:
        h = f.read(127)
        md_o, md_l, _, _, td_o, td_l = struct.unpack("<6Q", h[24:72])
        f.seek(md_o)
        layers = json.loads(gzip.decompress(f.read(md_l)))["vector_layers"]
        f.seek(td_o)
        return f.read(td_l), [l["id"] for l in layers]


def main():
    d = tempfile.mkdtemp(prefix="manby-test-")
    n = 20000
    points = shapely.points(
        [(-10 + 40 * (i * 7919 % n) / n, 35 + 25 * (i * 104729 % n) / n) for i in range(n)]
    )
    gdf = geopandas.GeoDataFrame(
        {
            "n": range(n),
            "kind": ["k%d" % (i % 13) for i in range(n)],
            "score": [i / 7 for i in range(n)],
        },
        geometry=points,
        crs="EPSG:4326",
    )
    path = os.path.join(d, "points.parquet")
    gdf.to_parquet(path)
    wkb = pa.table(
        {
            "n": pa.array(range(n), pa.int64()),
            "kind": gdf["kind"].tolist(),
            "score": gdf["score"].tolist(),
            "geometry": pa.array(shapely.to_wkb(points), pa.binary()),
        }
    )
    con = duckdb.connect()
    con.execute("INSTALL h3 FROM community")
    con.execute("LOAD h3")
    gdf_points = pa.table({"score": gdf["score"].tolist(), "kind": gdf["kind"].tolist(), "lon": list(gdf.geometry.x), "lat": list(gdf.geometry.y)})
    con.register("gdf_points", gdf_points)
    forms = {
        "file": path,
        "geopandas": gdf,
        "pyarrow": wkb,
        "duckdb": con.sql("SELECT * FROM wkb"),
        "polars": polars.from_arrow(wkb),
        "projected": gdf.to_crs(3035),
    }

    reference = None
    for name, data in forms.items():
        out = os.path.join(d, name, "points.pmtiles")
        os.makedirs(os.path.dirname(out))
        assert manby.tile(data, out, maximum_zoom="g", drop_densest_as_needed=True, quiet=True) == out
        t, layers = tiles(out)
        assert layers == ["points"], (name, layers)
        if name == "projected":
            # reprojected there and back, so near but not the same
            assert len(t) > 0
            continue
        if reference is None:
            reference = t
        assert t == reference, "%s makes different tiles" % name
        print("%s: the same tiles" % name)

    # a file that GDAL reads, through GeoPandas and through DuckDB
    sqlite = os.path.join(d, "points.sqlite")
    gdf.to_file(sqlite, driver="SQLite")
    assert manby._is_gdal(sqlite) and not manby._is_gdal(path)
    out = os.path.join(d, "sqlite", "points.pmtiles")
    os.makedirs(os.path.dirname(out))
    manby.tile(sqlite, out, maximum_zoom="g", drop_densest_as_needed=True, quiet=True)
    assert tiles(out) == (reference, ["points"]), "GDAL's file makes different tiles"
    os.makedirs(os.path.join(d, "duckdb-gdal"))
    parquet = manby._read_gdal(sqlite, None, os.path.join(d, "duckdb-gdal", "points"), use_geopandas=False)
    out = os.path.join(d, "duckdb-gdal", "points.pmtiles")
    manby.tile(parquet, out, maximum_zoom="g", drop_densest_as_needed=True, quiet=True)
    assert tiles(out) == (reference, ["points"]), "DuckDB's reading makes different tiles"
    print("a file that GDAL reads: the same tiles")

    # h3_sql: DuckDB's aggregates of each cell's points, on the hexagons
    sql = {"mean_score": "avg(score)", "kinds": "count(DISTINCT kind)"}
    for name, data in [("h3sql-gdf", gdf), ("h3sql-parquet", path), ("h3sql-arrow", wkb)]:
        out = os.path.join(d, name + ".mbtiles")
        manby.tile(data, out, layer="points", maximum_zoom=3, h3_no_points=True, h3_sql=sql, quiet=True)
        decoded = json.loads(subprocess.run([manby.binary(), "decode", out, "0", "0", "0"], capture_output=True, check=True).stdout)
        hexagons = decoded["features"][0]["features"]
        assert len(hexagons) > 0, name
        for h in hexagons:
            p = h["properties"]
            n, mean, kinds = con.execute(
                "SELECT count(*), avg(score), count(DISTINCT kind) FROM gdf_points "
                "WHERE h3_h3_to_string(h3_latlng_to_cell(lat, lon, 1)) = ?",
                [p["h3"]],
            ).fetchone()
            # the mean in the order DuckDB adds up the points, to the last digit
            same = (p["point_count"], p["kinds"]) == (n, kinds) and abs(p["mean_score"] - mean) <= 1e-12 * abs(mean)
            assert same, (name, p, n, mean, kinds)
    print("h3_sql: DuckDB's aggregates on the hexagons")

    out = os.path.join(d, "layers.pmtiles")
    manby.tile({"a": gdf, "b": path}, out, maximum_zoom=4, quiet=True)
    assert tiles(out)[1] == ["a", "b"]
    print("two layers")

    try:
        manby.tile(gdf, os.path.join(d, "bad.pmtiles"), maximum_zoom="x", quiet=True)
        raise AssertionError("no error")
    except manby.ManbyError as e:
        assert e.returncode != 0
    print("an error from manby raises ManbyError")

    with manby.serve(path_out := os.path.join(d, "geopandas", "points.pmtiles"), port=0, open=False) as server:
        tilejson = json.load(urllib.request.urlopen(server.tilejson["points"]))
        assert tilejson["vector_layers"][0]["id"] == "points", tilejson
        assert urllib.request.urlopen(server.url).status == 200
    assert server.process.poll() is not None
    print("serves %s" % path_out)

    report = manby.stats(path_out, layers=True, largest=3)
    assert sum(z["tiles"] for z in report["zooms"]) > 0
    assert all(z["layers"]["points"]["features"] > 0 for z in report["zooms"]), report
    assert len(report["largest"]) == 3
    print("stats")

    manby.run("--version")
    return 0


if __name__ == "__main__":
    sys.exit(main())
