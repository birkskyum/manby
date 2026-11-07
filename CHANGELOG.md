The changes in each version of manby, from the titles of the pull requests that made them.

## [0.1.4](https://github.com/birkskyum/manby/compare/v0.1.3...v0.1.4) - 2026-11-07

### Added

- `pip install manby` and `uvx manby`, from wheels on PyPI for macOS and Linux
- Binaries for macOS and Linux on the GitHub releases, which write MapLibre Tiles too

## [0.1.3](https://github.com/birkskyum/manby/compare/v0.1.2...v0.1.3) - 2026-11-07

### Fixed

- `--no-simplification-of-shared-nodes` in a third of the memory, and no panic on lines that collapse to one point ([#3](https://github.com/birkskyum/manby/pull/3))

## [0.1.2](https://github.com/birkskyum/manby/compare/v0.1.1...v0.1.2) - 2026-11-06

### Added

- `--as-needed-per-tile`, which fits each tile on its own rather than its whole zoom level

## [0.1.1](https://github.com/birkskyum/manby/compare/v0.1.0...v0.1.1) - 2026-11-06

### Faster or smaller

- The union of a simple polygon ring without wagyu's sweep, 5 to 9% less CPU on the census tracts

## [0.1.0](https://github.com/birkskyum/manby/releases/tag/v0.1.0) - 2026-11-05

The first release.
