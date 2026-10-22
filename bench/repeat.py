# Writes N copies of a GeoJSON-lines file, each moved north or south by a
# different amount, so that a larger input keeps the shape of real data.
# The copies spread over the latitudes that the file's bounds leave free.
#
#   python3 bench/repeat.py IN.geojsonl N > OUT.geojsonl
import json, sys

src, n = sys.argv[1], int(sys.argv[2])
features = [json.loads(line) for line in open(src) if line.strip()]


def lats(c):
    if isinstance(c[0], (int, float)):
        yield c[1]
    else:
        for x in c:
            yield from lats(x)


def moved(c, dy):
    if isinstance(c[0], (int, float)):
        return [c[0], round(c[1] + dy, 7)] + c[2:]
    return [moved(x, dy) for x in c]


low = min(min(lats(f["geometry"]["coordinates"])) for f in features)
high = max(max(lats(f["geometry"]["coordinates"])) for f in features)
up, down = 85 - high, -85 - low
w = sys.stdout.write
for k in range(n):
    dy = up - (up - down) * k / max(n - 1, 1)
    for f in features:
        g = f["geometry"]
        g2 = dict(g, coordinates=moved(g["coordinates"], dy))
        w(json.dumps(dict(f, geometry=g2), separators=(",", ":")) + "\n")
