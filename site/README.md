# manby's site

The landing page and documentation, built with [Starlight](https://starlight.astro.build) and Tailwind, and published with GitHub Pages at <https://birkskyum.github.io/manby/> by `.github/workflows/site.yml`.

```sh
pnpm install
pnpm dev      # http://localhost:4321/manby/
pnpm build    # into dist/
```

The pages are in `src/content/docs`, the hero with the benchmark chart in `src/components`. The numbers come from the README's tables, which `bench/` produces.
