"""manby builds vector tilesets from geographic features, like tippecanoe,
several times faster.

``tile`` makes a tileset from files, or from tables in memory: a DuckDB
query, a GeoDataFrame, or a Polars or PyArrow table. A table streams to the
manby program in Arrow batches, so a query result need not fit in memory.

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
"""

from __future__ import annotations

import errno
import json
import os
import shutil
import subprocess
import sys
import sysconfig
import tempfile
import threading
import time
from typing import Any, Mapping, Sequence

__all__ = ["tile", "run", "binary", "ManbyError"]


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
    """Writes a table to a named pipe that manby reads."""

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
    for name, d in layers.items():
        stream = _arrow(d)
        if stream is None:
            if name is None:
                argv.append(os.fspath(d))
            else:
                argv.append("-L" + json.dumps({"file": os.fspath(d), "layer": name}))
            continue
        if tmp is None:
            tmp = tempfile.mkdtemp(prefix="manby-")
        # named as the tileset, which an unnamed layer is named after
        stem = os.path.basename(os.fspath(output)).rsplit(".", 1)[0] or "layer"
        path = os.path.join(tmp, str(len(feeds)), stem + ".arrows")
        os.mkdir(os.path.dirname(path))
        os.mkfifo(path)
        source = {"file": path, "format": "arrow"}
        if name is not None:
            source["layer"] = name
        argv.append("-L" + json.dumps(source))
        feeds.append((path, stream))

    try:
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


def main() -> None:
    """Runs the manby program with this process's arguments."""
    argv = [binary(), *sys.argv[1:]]
    if os.name == "posix":
        os.execv(argv[0], argv)
    sys.exit(subprocess.call(argv))
