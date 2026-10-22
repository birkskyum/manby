# Writes N clustered points as newline-delimited GeoJSON: Gaussian blobs around
# random centers, so density varies the way real point data does.
import random, sys
random.seed(42)
n = int(sys.argv[1])
centers = [(random.uniform(-125, -67), random.uniform(25, 49), random.uniform(0.02, 1.5)) for _ in range(2000)]
w = sys.stdout.write
for i in range(n):
    cx, cy, s = centers[random.randrange(len(centers))]
    x = max(-179.9, min(179.9, random.gauss(cx, s)))
    y = max(-85, min(85, random.gauss(cy, s)))
    w('{"type":"Feature","properties":{"id":%d,"kind":"%s","value":%.3f},"geometry":{"type":"Point","coordinates":[%.6f,%.6f]}}\n' % (i, "abcdef"[i % 6], random.random() * 1000, x, y))
