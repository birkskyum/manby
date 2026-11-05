// The benchmark figures, once, for the charts and tables of
// reference/benchmarks and the chart on the landing page: seconds and peak
// resident memory in MB, medians of three runs (the larger inputs, once).

export type Figures = [seconds: number, megabytes: number];

export const runs = {
	points: {
		name: 'Points, dropping the densest as needed',
		options: '-zg --drop-densest-as-needed',
	},
	'points-cluster': {
		name: 'Points, clustered within 10 pixels, values summed',
		options:
			'-zg -r1 --cluster-distance=10 --cluster-densest-as-needed --accumulate-attribute=value:sum',
	},
	'cluster-densest': {
		name: 'Points, clustering the densest as needed',
		options: '-zg --cluster-densest-as-needed',
	},
	'cluster-r1-sum': {
		name: 'Points, clustered at every zoom, values summed',
		options: '-zg -r1 --cluster-densest-as-needed --accumulate-attribute=value:sum',
	},
	'roads-z14': { name: 'Roads to z14', options: '-z14 -Z0 --drop-densest-as-needed' },
	'tracts-z12': { name: 'Tracts to z12', options: '-z12 -Z0 --coalesce-densest-as-needed' },
	tracts: {
		name: 'Tracts, coalescing the densest as needed',
		options: '-zg --coalesce-densest-as-needed',
	},
} as const;

export type Run = keyof typeof runs;
type Pair = { tippecanoe: Figures; manby: Figures };

/** An Apple M5 Max with 18 cores and 128 GB. */
export const macos: Record<Run, Pair> = {
	points: { tippecanoe: [61.4, 2276], manby: [3.44, 919] },
	'points-cluster': { tippecanoe: [54.4, 1318], manby: [3.88, 891] },
	'cluster-densest': { tippecanoe: [59.8, 2330], manby: [4.07, 916] },
	'cluster-r1-sum': { tippecanoe: [87.7, 3443], manby: [7.82, 1115] },
	'roads-z14': { tippecanoe: [11.8, 327], manby: [0.81, 177] },
	'tracts-z12': { tippecanoe: [46.6, 566], manby: [3.78, 338] },
	tracts: { tippecanoe: [24.8, 554], manby: [2.27, 366] },
};

/** A Lenovo Yoga Slim 7x with a Snapdragon X Elite, 12 cores and 32 GB. */
export const linux: Record<Run, Pair> = {
	points: { tippecanoe: [88.5, 1088], manby: [5.86, 994] },
	'points-cluster': { tippecanoe: [82.5, 1028], manby: [6.02, 974] },
	'cluster-densest': { tippecanoe: [89.6, 1165], manby: [6.37, 1000] },
	'cluster-r1-sum': { tippecanoe: [133.6, 1498], manby: [13.9, 1152] },
	'roads-z14': { tippecanoe: [59.0, 240], manby: [1.51, 236] },
	'tracts-z12': { tippecanoe: [196.8, 456], manby: [7.77, 360] },
	tracts: { tippecanoe: [36.1, 451], manby: [4.46, 379] },
};

/** With --memory-budget=1G for manby, once each. */
export const larger: { name: string; system: string; tippecanoe: Figures; manby: Figures }[] = [
	{ name: '40 million points', system: 'macOS', tippecanoe: [652, 10206], manby: [79, 1139] },
	{ name: '40 million points', system: 'Linux', tippecanoe: [1025, 6379], manby: [121, 1188] },
	{ name: 'The tracts 20 times over', system: 'macOS', tippecanoe: [564, 4810], manby: [137, 1022] },
	{ name: 'The tracts 20 times over', system: 'Linux', tippecanoe: [854, 3730], manby: [269, 1206] },
];

/** On the Mac, tylertoo from its main branch (6c3cec4) reading GeoParquet. */
export const tylertoo: { name: string; tylertoo: Figures; manby: Figures }[] = [
	{ name: 'Points to z6, dropping the densest as needed', tylertoo: [5.61, 1329], manby: [3.98, 913] },
	{ name: 'Roads to z14, dropping the densest as needed', tylertoo: [1.85, 1447], manby: [0.89, 180] },
	{ name: 'Tracts to z12', tylertoo: [3.39, 1420], manby: [4.07, 342] },
	{ name: 'Tracts to z7, coalescing the densest as needed', tylertoo: [0.9, 1139], manby: [2.69, 377] },
];

export type Program = { key: string; label: string; tone: 'manby' | 'other' };

export type Chart = {
	title: string;
	/** Which of the two figures the bars show. */
	measure: 'time' | 'memory';
	programs: Program[];
	rows: { label: string; values: Record<string, Figures> }[];
	/** Show how many times faster manby is than the other program. */
	speedup?: boolean;
};

const againstTippecanoe: Program[] = [
	{ key: 'manby', label: 'manby', tone: 'manby' },
	{ key: 'tippecanoe', label: 'tippecanoe', tone: 'other' },
];

const fromRuns = (figures: Record<Run, Pair>) =>
	(Object.keys(runs) as Run[]).map((k) => ({ label: runs[k].name, values: figures[k] }));

export const charts = {
	macosTime: {
		title: 'Seconds to make each tileset, on macOS',
		measure: 'time',
		programs: againstTippecanoe,
		rows: fromRuns(macos),
		speedup: true,
	},
	macosMemory: {
		title: 'Peak memory in MB, on macOS',
		measure: 'memory',
		programs: againstTippecanoe,
		rows: fromRuns(macos),
	},
	linuxTime: {
		title: 'Seconds to make each tileset, on Linux',
		measure: 'time',
		programs: againstTippecanoe,
		rows: fromRuns(linux),
		speedup: true,
	},
	linuxMemory: {
		title: 'Peak memory in MB, on Linux',
		measure: 'memory',
		programs: againstTippecanoe,
		rows: fromRuns(linux),
	},
	largerTime: {
		title: 'Seconds, with a 1 GB memory budget for manby',
		measure: 'time',
		programs: againstTippecanoe,
		rows: larger.map((r) => ({ label: `${r.name}, ${r.system}`, values: r })),
		speedup: true,
	},
	largerMemory: {
		title: 'Peak memory in MB, with a 1 GB memory budget for manby',
		measure: 'memory',
		programs: againstTippecanoe,
		rows: larger.map((r) => ({ label: `${r.name}, ${r.system}`, values: r })),
	},
	tylertooTime: {
		title: 'Seconds to make each tileset, on macOS',
		measure: 'time',
		programs: [
			{ key: 'manby', label: 'manby', tone: 'manby' },
			{ key: 'tylertoo', label: 'tylertoo', tone: 'other' },
		],
		rows: tylertoo.map((r) => ({ label: r.name, values: r })),
	},
	tylertooMemory: {
		title: 'Peak memory in MB, on macOS',
		measure: 'memory',
		programs: [
			{ key: 'manby', label: 'manby', tone: 'manby' },
			{ key: 'tylertoo', label: 'tylertoo', tone: 'other' },
		],
		rows: tylertoo.map((r) => ({ label: r.name, values: r })),
	},
} satisfies Record<string, Chart>;
