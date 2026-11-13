"""manby builds vector tilesets from geographic features, like tippecanoe,
several times faster.

``tile`` makes a tileset from files, or from tables in memory: a DuckDB
query, a GeoDataFrame, or a Polars or PyArrow table. A table streams to the
manby program in Arrow batches, so a query result need not fit in memory. A
file in a format that GDAL reads and manby does not, such as KML, GPX or
OpenStreetMap, is read with GeoPandas, or with DuckDB's spatial extension.

    import duckdb, manby

    manby.tile("roads.geojson", "roads.pmtiles", "-zg", "--drop-densest-as-needed")
    manby.tile(
        duckdb.sql("SELECT name, geometry FROM 'places.parquet' WHERE country = 'DE'"),
        "places.pmtiles",
        maximum_zoom=14,
        drop_densest_as_needed=True,
    )

The options are tippecanoe's: ``maximum_zoom=14`` is ``--maximum-zoom=14``,
``drop_densest_as_needed=True`` is ``--drop-densest-as-needed``, and a list
repeats an option. Arguments after the output are passed as they are.

``serve`` serves tilesets with a map of them, in the browser or in a notebook,
and ``stats`` reports the sizes of their tiles.
"""

from __future__ import annotations

import atexit
import errno
import json
import os
import re
import shutil
import subprocess
import sys
import sysconfig
import tempfile
import threading
import time
import webbrowser
from typing import Any, Mapping, Sequence

__all__ = ["tile", "serve", "stats", "run", "binary", "Server", "ManbyError"]


class ManbyError(RuntimeError):
    """manby exited with an error. ``returncode`` is its exit status, as
    tippecanoe's: 100 when the tileset is complete only to a lower zoom."""

    def __init__(self, returncode: int, args: Sequence[str]):
        self.returncode = returncode
        super().__init__(
            "manby exited with status %d: %s" % (returncode, " ".join(args))
        )


def binary() -> str:
    """The path of the manby program that this package installed."""
    name = "manby.exe" if os.name == "nt" else "manby"
    dirs = []
    for scheme in (None, "%s_user" % os.name):
        try:
            dirs.append(
                sysconfig.get_path("scripts", scheme)
                if scheme
                else sysconfig.get_path("scripts")
            )
        except KeyError:
            pass
    # pip install --target puts the program beside the package
    here = os.path.dirname(os.path.abspath(__file__))
    dirs.append(os.path.join(os.path.dirname(here), "bin"))
    for d in dirs:
        if d:
            path = os.path.join(d, name)
            if os.path.isfile(path) and os.access(path, os.X_OK):
                return path
    path = shutil.which(name)
    if path:
        return path
    raise FileNotFoundError("the manby program was not found next to its package")


def run(*args: str) -> None:
    """Runs manby with ``args``, as on the command line. For example,
    ``run("join", "-o", "all.pmtiles", "a.pmtiles", "b.pmtiles")``."""
    argv = [binary(), *map(str, args)]
    code = subprocess.call(argv)
    if code != 0:
        raise ManbyError(code, argv[1:])


def _flags(options: Mapping[str, Any]) -> list:
    out = []
    for key, value in options.items():
        flag = "--" + key.replace("_", "-")
        if value is None or value is False:
            continue
        if value is True:
            out.append(flag)
        elif isinstance(value, (list, tuple)):
            out.extend("%s=%s" % (flag, v) for v in value)
        else:
            out.append("%s=%s" % (flag, value))
    return out


_GDAL = re.compile(
    r"\.(kml|kmz|gml|gpx|osm|osm\.pbf|osm\.bz2|gdb|tab|mif|dxf|dgn|sqlite|"
    r"xlsx|xls|ods|topojson|vrt|gmt|jml|nc|e00|shp\.zip|gpkg\.zip)$"
)


def _is_gdal(data: Any) -> bool:
    """Whether ``data`` is a file in a format that GDAL reads and manby does not."""
    if not isinstance(data, (str, os.PathLike)):
        return False
    return _GDAL.search(os.fspath(data).rstrip("/\\").lower()) is not None


def _gdal_stem(path: str) -> str:
    """A file's name without its extension, as manby names its layer."""
    name = os.path.basename(path.rstrip("/\\"))
    return re.sub(r"\.(osm\.[a-z0-9]+|[a-z]+\.zip|[^.]*)$", "", name, flags=re.I) or "layer"


def _quote(x: str) -> str:
    return "'%s'" % x.replace("'", "''")


def _read_gdal(path: str, layer: str | None, out: str, use_geopandas: bool = True) -> Any:
    """The layer ``layer`` of a file that GDAL reads, or its first layer: a
    GeoDataFrame, or with DuckDB the name of a Parquet file written as ``out``."""
    geopandas = None
    if use_geopandas:
        try:
            import geopandas
        except ImportError:
            pass
    if geopandas is not None:
        names = list(geopandas.list_layers(path)["name"])
        return geopandas.read_file(path, layer=layer if layer in names else names[0])
    try:
        import duckdb
    except ImportError:
        raise ImportError(
            "manby reads %s with GeoPandas or DuckDB: pip install geopandas"
            % os.path.basename(path)
        ) from None
    con = duckdb.connect()
    try:
        con.execute("INSTALL spatial")
        con.execute("LOAD spatial")
        meta = con.execute(
            "SELECT l.name, l.geometry_fields[1].name, l.geometry_fields[1].crs.auth_name, "
            "l.geometry_fields[1].crs.auth_code, [f.name FOR f IN l.fields] "
            "FROM (SELECT unnest(layers) AS l FROM ST_Read_Meta(?))",
            [path],
        ).fetchall()
        row = next((m for m in meta if m[0] == layer), meta[0] if meta else None)
        # ST_Read names the geometry after the source's column, or geom
        column = '"%s"' % (row[1] if row and row[1] else "geom").replace('"', '""')
        crs = "%s:%s" % (row[2], row[3]) if row and row[2] and row[3] else "EPSG:4326"
        if crs in ("EPSG:4326", "OGC:CRS84"):
            geometry = column
        else:
            geometry = "ST_Transform(%s, %s, 'EPSG:4326', always_xy := true)" % (column, _quote(crs))
        file = out + ".parquet"
        source = _quote(path) + (", layer = " + _quote(row[0]) if row else "")
        # the layer's fields, without the feature ids that DuckDB 2 adds
        fields = (
            "".join('"%s", ' % f.replace('"', '""') for f in row[4])
            if row
            else "* EXCLUDE (%s), " % column
        )
        con.execute(
            "COPY (SELECT %sST_AsWKB(%s) AS geometry FROM ST_Read(%s)) TO %s (FORMAT parquet)"
            % (fields, geometry, source, _quote(file))
        )
        return file
    finally:
        con.close()


def _is_geodataframe(data: Any) -> bool:
    return any(
        c.__name__ == "GeoDataFrame" and c.__module__.startswith("geopandas")
        for c in type(data).__mro__
    )


def _arrow(data: Any) -> Any:
    """``data`` as an object with an Arrow stream, or None if it is a path."""
    if isinstance(data, (str, bytes, os.PathLike)):
        return None
    if _is_geodataframe(data):
        crs = data.crs
        if crs is not None and crs.to_epsg() not in (4326, 3857):
            data = data.to_crs(4326)
        return data.to_arrow(geometry_encoding="WKB")
    if hasattr(data, "__arrow_c_stream__"):
        return data
    raise TypeError(
        "manby.tile takes a file name, a GeoDataFrame, or a table with an Arrow "
        "stream, such as a DuckDB relation or a Polars or PyArrow table, not %s"
        % type(data).__name__
    )


def _write_ipc(data: Any, f: Any) -> None:
    """Writes ``data`` to ``f`` as an Arrow IPC stream."""
    try:
        import pyarrow as pa
    except ImportError:
        pa = None
    if pa is not None:
        reader = pa.RecordBatchReader.from_stream(data)
        with pa.ipc.new_stream(f, reader.schema) as w:
            for batch in reader:
                w.write_batch(batch)
        return
    if hasattr(data, "write_ipc_stream"):  # Polars
        data.write_ipc_stream(f)
        return
    raise ImportError("manby needs pyarrow to send this table: pip install pyarrow")


class _Feed(threading.Thread):
    """Writes a table to a named pipe that manby reads, on Unix."""

    def __init__(self, path: str, data: Any, proc: subprocess.Popen):
        super().__init__(daemon=True)
        self.path, self.data, self.proc = path, data, proc
        self.error = None  # type: BaseException | None

    def run(self) -> None:
        try:
            # manby opens the pipes one at a time, in order, and may stop
            # before it opens this one
            while True:
                try:
                    fd = os.open(self.path, os.O_WRONLY | os.O_NONBLOCK)
                    break
                except OSError as e:
                    if e.errno != errno.ENXIO:
                        raise
                    if self.proc.poll() is not None:
                        return
                    time.sleep(0.005)
            os.set_blocking(fd, True)
            with open(fd, "wb") as f:
                _write_ipc(self.data, f)
        except BrokenPipeError:
            pass  # manby stopped; its status says why
        except BaseException as e:
            self.error = e


def tile(
    data: Any,
    output: str | os.PathLike,
    *args: str,
    layer: str | None = None,
    force: bool = True,
    **options: Any,
) -> str:
    """Makes the tileset ``output`` from ``data``, and gives its path.

    ``data`` is a file name, a GeoDataFrame, a table with an Arrow stream (a
    DuckDB relation, a Polars or PyArrow table), or a dict of layer names to
    any of these. A table's geometry is the column that its GeoArrow or
    GeoParquet metadata names, or a column named ``geometry`` in well-known
    binary. ``layer`` names the layer of a single input. ``force`` overwrites
    an existing tileset. The other options are tippecanoe's long options.
    """
    layers = data if isinstance(data, Mapping) else {layer: data}
    argv = ["-o", os.fspath(output)]
    if force:
        argv.append("--force")
    argv += _flags(options)
    argv += [str(a) for a in args]

    feeds = []
    tmp = None
    try:
        for i, (name, d) in enumerate(layers.items()):
            if _is_gdal(d):
                if tmp is None:
                    tmp = tempfile.mkdtemp(prefix="manby-")
                source = os.fspath(d)
                if name is None:
                    # named after the file, as manby names a file's layer
                    name = _gdal_stem(source)
                os.mkdir(os.path.join(tmp, "gdal-%d" % i))
                d = _read_gdal(source, name, os.path.join(tmp, "gdal-%d" % i, _gdal_stem(source)))
            stream = _arrow(d)
            if stream is None:
                if name is None:
                    argv.append(os.fspath(d))
                else:
                    argv.append(
                        "-L" + json.dumps({"file": os.fspath(d), "layer": name})
                    )
                continue
            if tmp is None:
                tmp = tempfile.mkdtemp(prefix="manby-")
            # named as the tileset, which an unnamed layer is named after
            stem = os.path.basename(os.fspath(output)).rsplit(".", 1)[0] or "layer"
            path = os.path.join(tmp, str(i), stem + ".arrows")
            os.mkdir(os.path.dirname(path))
            if hasattr(os, "mkfifo"):
                os.mkfifo(path)
                feeds.append((path, stream))
            else:
                # Windows has no named pipes among its files, so the table is
                # written out first, as the R package writes its tables
                with open(path, "wb") as f:
                    _write_ipc(stream, f)
            source = {"file": path, "format": "arrow"}
            if name is not None:
                source["layer"] = name
            argv.append("-L" + json.dumps(source))

        proc = subprocess.Popen([binary(), *argv])
        threads = [_Feed(path, stream, proc) for path, stream in feeds]
        for t in threads:
            t.start()
        try:
            code = proc.wait()
        except BaseException:
            proc.kill()
            raise
        for t in threads:
            t.join()
        for t in threads:
            if t.error is not None:
                raise t.error
        if code != 0:
            raise ManbyError(code, argv)
    finally:
        if tmp is not None:
            shutil.rmtree(tmp, ignore_errors=True)
    return os.fspath(output)


class Server:
    """A running ``manby serve``. ``url`` is the map, ``tilejson`` the
    TileJSON of each tileset by name, and ``stop()`` ends it. In a notebook,
    the server shows as its map."""

    def __init__(self, proc: subprocess.Popen, url: str, tilejson: Mapping[str, str]):
        self.process = proc
        self.url = url
        self.tilejson = dict(tilejson)

    def stop(self) -> None:
        """Stops serving."""
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(5)
            except subprocess.TimeoutExpired:
                self.process.kill()

    def __enter__(self) -> "Server":
        return self

    def __exit__(self, *exc: Any) -> None:
        self.stop()

    def __repr__(self) -> str:
        state = "serving" if self.process.poll() is None else "stopped"
        return "<manby.Server %s %s>" % (state, self.url)

    def _repr_html_(self) -> str:
        return '<iframe src="%s" style="width:100%%;height:480px;border:0"></iframe>' % self.url


def serve(
    *tilesets: str | os.PathLike,
    port: int = 8080,
    host: str = "127.0.0.1",
    open: bool | None = None,
) -> Server:
    """Serves PMTiles archives, MBTiles databases or directories of tiles
    until ``stop()``, with a map of them at ``url``. ``open`` opens the map in
    a browser, as it does by default outside a notebook, where the server
    shows as its map instead. ``port=0`` takes any free port. A tileset made
    again while it is served shows when the map is reloaded."""
    argv = [binary(), "serve", "--port", str(port), "--host", host]
    argv += [os.fspath(t) for t in tilesets]
    proc = subprocess.Popen(argv, stdout=subprocess.PIPE, text=True)
    url = proc.stdout.readline().strip()
    if not url:
        raise ManbyError(proc.wait(), argv[1:])
    tilejson = {}
    for _ in tilesets:
        name, _, address = proc.stdout.readline().strip().partition(": ")
        tilejson[name] = address
    server = Server(proc, url, tilejson)
    atexit.register(server.stop)
    if open is None:
        open = "ipykernel" not in sys.modules
    if open:
        webbrowser.open(url)
    return server


def stats(tileset: str | os.PathLike, layers: bool = False, largest: int = 10) -> dict:
    """The sizes of a tileset's tiles at each zoom, as stored, and the
    ``largest`` tiles. ``layers`` also counts the features of each layer at
    each zoom, from every tile. The result is ``manby stats --json``'s."""
    argv = [binary(), "stats", "--json", "--largest=%d" % largest]
    if layers:
        argv.append("--layers")
    argv.append(os.fspath(tileset))
    done = subprocess.run(argv, stdout=subprocess.PIPE, text=True)
    if done.returncode != 0:
        raise ManbyError(done.returncode, argv[1:])
    return json.loads(done.stdout)


def main() -> None:
    """Runs the manby program with this process's arguments."""
    argv = [binary(), *sys.argv[1:]]
    if os.name == "posix":
        os.execv(argv[0], argv)
    sys.exit(subprocess.call(argv))
