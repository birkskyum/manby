"""Writes a CSV keyed on the census tracts' GEOID, with made-up numbers and a
quoted label for each tract, for joining to the tracts with tile-join -c.

    python3 tracts-csv.py cb_2023_us_tract_500k.geojsonl > tracts.csv
"""

import json
import random
import sys

random.seed(1)
print("GEOID,population,households,income,density,label")
with open(sys.argv[1]) as f:
    for line in f:
        props = json.loads(line)["properties"]
        population = random.randint(500, 9000)
        households = population // random.randint(2, 4)
        income = random.randint(18000, 250000)
        density = round(population / max(props["ALAND"], 1) * 1e6, 3)
        label = props["NAMELSAD"] + ", " + props["NAMELSADCO"]
        print(f'{props["GEOID"]},{population},{households},{income},{density},"{label}"')
