// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import tailwindcss from '@tailwindcss/vite';

// Served by GitHub Pages from the manby repository; with a domain of its
// own, `site` becomes that domain and `base` goes.
export default defineConfig({
	site: 'https://birkskyum.github.io',
	base: '/manby',
	integrations: [
		starlight({
			title: 'manby',
			description:
				'Builds vector tilesets from geographic features. The same tiles as tippecanoe, eight to eighteen times faster, in less memory.',
			social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/birkskyum/manby' }],
			customCss: ['@fontsource-variable/inter', './src/styles/global.css'],
			components: { SiteTitle: './src/components/SiteTitle.astro' },
			expressiveCode: {
				themes: ['github-dark', 'github-light'],
				styleOverrides: { borderRadius: '0.75rem' },
			},
			sidebar: [
				{ label: 'Why manby?', slug: 'why' },
				{ label: 'Getting started', slug: 'getting-started' },
				{
					label: 'Guides',
					items: [
						{ label: 'Inputs larger than memory', slug: 'guides/memory' },
						{ label: 'Joining tilesets', slug: 'guides/join' },
						{ label: "tippecanoe's other programs", slug: 'guides/programs' },
						{ label: 'MapLibre Tiles', slug: 'guides/mlt' },
						{ label: 'Other formats, with terrio', slug: 'guides/other-formats' },
					],
				},
				{
					label: 'Reference',
					items: [
						{ label: 'Benchmarks', slug: 'reference/benchmarks' },
						{ label: 'How it works', slug: 'reference/how' },
						{ label: 'Differences from tippecanoe', slug: 'reference/differences' },
					],
				},
			],
		}),
	],
	vite: {
		plugins: [tailwindcss()],
	},
});
