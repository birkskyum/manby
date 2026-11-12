// Writes points.json: points and what Supercluster 8.0.1 makes of them, for
// src/ingest/supercluster.rs's test. Run with supercluster@8.0.1 installed:
//   NODE_PATH=... node make.mjs > points.json
import Supercluster from 'supercluster';

// clumps of points around a few centers, and some spread out
let seed = 42;
const random = () => {
    seed = (seed * 1103515245 + 12345) % 2147483648;
    return seed / 2147483648;
};
const points = [];
const centers = [[-73.9, 40.7], [2.35, 48.85], [139.7, 35.7], [-0.1, 51.5], [151.2, -33.9]];
for (let i = 0; i < 2500; i++) {
    if (i % 5 === 4) {
        points.push([random() * 360 - 180, random() * 170 - 85]);
    } else {
        const c = centers[i % centers.length];
        const spread = Math.pow(10, -random() * 3) * 5;
        points.push([c[0] + (random() - 0.5) * spread, c[1] + (random() - 0.5) * spread]);
    }
}

const options = {radius: 50, extent: 512, minPoints: 2, maxZoom: 12};
const index = new Supercluster(options).load(points.map(p => ({
    type: 'Feature', properties: {}, geometry: {type: 'Point', coordinates: p}
})));

// the zooms at which each point and each cluster is in Supercluster's trees
const alone = points.map(() => options.maxZoom + 1);
const clusters = new Map();
for (let z = 0; z <= options.maxZoom; z++) {
    const data = index.trees[z].data;
    for (let k = 0; k < data.length; k += 6) {
        if (data[k + 5] > 1) {
            const id = data[k + 3];
            const c = clusters.get(id) || {id, count: data[k + 5], x: data[k], y: data[k + 1], minzoom: z, maxzoom: z};
            c.minzoom = Math.min(c.minzoom, z);
            c.maxzoom = Math.max(c.maxzoom, z);
            clusters.set(id, c);
        } else {
            const i = data[k + 3];
            alone[i] = Math.min(alone[i], z);
        }
    }
}
const expansion = {};
for (const id of clusters.keys()) expansion[id] = index.getClusterExpansionZoom(id);

console.log(JSON.stringify({options, points, alone_from: alone, clusters: [...clusters.values()], expansion}));
