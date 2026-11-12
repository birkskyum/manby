// Writes places.json: what MapLibre would show at each zoom of the populated
// places clustered by Supercluster 8.0.1, for tests/supercluster.rs. Run with
// supercluster@8.0.1 installed:
//   node places.mjs ../../crates/tippecanoe-rs/tests/ne_110m_populated_places/in.json > places.json
import Supercluster from 'supercluster';
import fs from 'fs';

const places = JSON.parse(fs.readFileSync(process.argv[2]));
const points = places.features.filter(f => f.geometry && f.geometry.type === 'Point');
const index = new Supercluster({radius: 50, extent: 512, maxZoom: 5}).load(points);
const zooms = [];
for (let z = 0; z <= 6; z++) {
    const keys = index.getClusters([-180, -90, 180, 90], z).map(f => {
        const p = f.properties;
        return p.cluster
            ? `cluster ${p.cluster_id} ${p.point_count} ${p.point_count_abbreviated} ${index.getClusterExpansionZoom(p.cluster_id)}`
            : `point ${p.NAME}`;
    });
    zooms.push([...new Set(keys)].sort());
}
console.log(JSON.stringify(zooms));
