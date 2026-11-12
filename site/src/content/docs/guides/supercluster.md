---
title: Clusters as MapLibre makes them
description: --supercluster clusters points with Supercluster, as a MapLibre GeoJSON source does, with its properties and the zoom at which each cluster splits.
---

`--supercluster` clusters each layer's points with [Supercluster](https://github.com/mapbox/supercluster) 8's algorithm, as a MapLibre GeoJSON source with `cluster: true` does, and writes the clusters into the tiles. A map then shows the same clusters from vector tiles as from the GeoJSON, without loading every point.

```sh
manby -z14 --supercluster -o places.pmtiles places.geojson
```

At each zoom, the tiles hold the clusters and points that Supercluster has at that zoom. Each point shows on its own from the zoom after the one where it was last in a cluster, and with all of its attributes. Each cluster is a point feature with Supercluster's properties, and its `cluster_id` is also the feature's id.

| Property | |
| --- | --- |
| `cluster` | `true` |
| `cluster_id` | Supercluster's id for the cluster |
| `point_count` | The points in the cluster |
| `point_count_abbreviated` | The count for labels, such as `850`, `1.2k` or `12k` |
| `cluster_expansion_zoom` | The zoom at which the cluster splits, what `getClusterExpansionZoom` returns |

`cluster_expansion_zoom` is what a click on a cluster zooms to:

```js
map.on('click', 'clusters', (e) => {
    const f = e.features[0];
    map.easeTo({center: f.geometry.coordinates, zoom: f.properties.cluster_expansion_zoom});
});
```

## Options

- **`--supercluster-radius=PIXELS`** sets the radius of a cluster in pixels of a 512-pixel tile, as MapLibre's `clusterRadius` does. It is 50 unless given.
- **`--supercluster-min-points=N`** sets the fewest points that make a cluster, as `clusterMinPoints` does. It is 2 unless given.
- **`-k`, `--cluster-maxzoom=Z`** sets the last zoom with clusters, as `clusterMaxZoom` does. It is one below the maximum zoom unless given, and must be given with `-zg`.

Supercluster clusters the points in the order they are read, so another order can make other clusters. Points of a MultiPoint are not clustered. Nothing is dropped by rate (`-r1`), since Supercluster places every point and cluster, and tippecanoe's own clustering (`-K`) and `--h3` do not work with `--supercluster`.

Clustering a million points takes about a second.
