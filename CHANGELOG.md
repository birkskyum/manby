The changes in each version of manby, from the titles of the pull requests that made them.


## [0.2.7](https://github.com/birkskyum/manby/compare/v0.2.6...v0.2.7) - 2026-11-13

### Added

- Build manby into the R package from CRAN ([#76](https://github.com/birkskyum/manby/pull/76))
- SQL aggregates for H3 hexagons ([#75](https://github.com/birkskyum/manby/pull/75))
- A view of H3 hexagons in manby serve's map ([#68](https://github.com/birkskyum/manby/pull/68))
- H3 medians, modes, fades, resolutions and hexagons only ([#65](https://github.com/birkskyum/manby/pull/65))

### Fixed

- The CRAN package's workspace crates from the checkout ([#77](https://github.com/birkskyum/manby/pull/77))
- Keep manby serve's Meta as 0.2.6 has it ([#74](https://github.com/birkskyum/manby/pull/74))
- Build with Rust 1.90, with h3o 0.9.4 ([#69](https://github.com/birkskyum/manby/pull/69))

### Dependencies

- Bump the cargo group with 2 updates ([#70](https://github.com/birkskyum/manby/pull/70))

## [0.2.6](https://github.com/birkskyum/manby/compare/v0.2.5...v0.2.6) - 2026-11-12

### Added

- Read shapefiles ([#64](https://github.com/birkskyum/manby/pull/64))
- Read GeoPackage files ([#62](https://github.com/birkskyum/manby/pull/62))

## [0.2.5](https://github.com/birkskyum/manby/compare/v0.2.4...v0.2.5) - 2026-11-12

### Added

- Run the R package in webR ([#60](https://github.com/birkskyum/manby/pull/60))

## [0.2.4](https://github.com/birkskyum/manby/compare/v0.2.3...v0.2.4) - 2026-11-12

### Added

- --supercluster, clusters as MapLibre makes them ([#58](https://github.com/birkskyum/manby/pull/58))

### Fixed

- Name a damaged PMTiles archive in manby join's message ([#57](https://github.com/birkskyum/manby/pull/57))

## [0.2.3](https://github.com/birkskyum/manby/compare/v0.2.2...v0.2.3) - 2026-11-11

### Added

- --shard and manby merge, builds split across machines ([#54](https://github.com/birkskyum/manby/pull/54))

### Fixed

- Keep the shard record off the public tileset Meta ([#56](https://github.com/birkskyum/manby/pull/56))

## [0.2.2](https://github.com/birkskyum/manby/compare/v0.2.1...v0.2.2) - 2026-11-11

### Added

- Release manby for Windows ([#53](https://github.com/birkskyum/manby/pull/53))
- Build and test manby on Windows ([#50](https://github.com/birkskyum/manby/pull/50))

## [0.2.1](https://github.com/birkskyum/manby/compare/v0.2.0...v0.2.1) - 2026-11-10

### Added

- Take each feature's zooms from attributes with --use-attribute-for-minzoom and --use-attribute-for-maxzoom ([#48](https://github.com/birkskyum/manby/pull/48))

## [0.2.0](https://github.com/birkskyum/manby/compare/v0.1.11...v0.2.0) - 2026-11-09

### Changed

- [**breaking**] A public API of run, join and the commands, with the rest hidden ([#42](https://github.com/birkskyum/manby/pull/42))

## [0.1.11](https://github.com/birkskyum/manby/compare/v0.1.10...v0.1.11) - 2026-11-09

### Added

- Like, ilike and intersects in feature filters ([#31](https://github.com/birkskyum/manby/pull/31))
- --largest-maximum-zoom-guess caps the maxzoom that -zg chooses ([#35](https://github.com/birkskyum/manby/pull/35))

### Fixed

- Keep high zooms of large polygons within memory ([#37](https://github.com/birkskyum/manby/pull/37))
- Say that manby needs Rust 1.94, and build with it in CI ([#36](https://github.com/birkskyum/manby/pull/36))

## [0.1.10](https://github.com/birkskyum/manby/compare/v0.1.9...v0.1.10) - 2026-11-09

### Fixed

- Keep reading Arrow and FlatGeobuf within the memory budget ([#30](https://github.com/birkskyum/manby/pull/30))

## [0.1.9](https://github.com/birkskyum/manby/compare/v0.1.8...v0.1.9) - 2026-11-08

### Added

- Count an attribute's values in clusters with --cluster-categories ([#25](https://github.com/birkskyum/manby/pull/25))

## [0.1.8](https://github.com/birkskyum/manby/compare/v0.1.7...v0.1.8) - 2026-11-08

### Added

- Bin points into H3 hexagons below a base zoom with --h3 ([#24](https://github.com/birkskyum/manby/pull/24))
- Read coordinates in any EPSG or ESRI CRS, from -s or GeoParquet metadata ([#22](https://github.com/birkskyum/manby/pull/22))

## [0.1.7](https://github.com/birkskyum/manby/compare/v0.1.6...v0.1.7) - 2026-11-08

### Added

- Read GeoParquet from object storage and the web, and pick rows with --bbox ([#21](https://github.com/birkskyum/manby/pull/21))
- Report a tileset's tile sizes and features at each zoom with manby stats ([#19](https://github.com/birkskyum/manby/pull/19))

## [0.1.6](https://github.com/birkskyum/manby/compare/v0.1.5...v0.1.6) - 2026-11-07

### Added

- Serve tilesets with a map of them, from the command line, R and Python ([#18](https://github.com/birkskyum/manby/pull/18))

## [0.1.5](https://github.com/birkskyum/manby/compare/v0.1.4...v0.1.5) - 2026-11-07

### Added

- Tile sf data frames and DuckDB tables from R ([#15](https://github.com/birkskyum/manby/pull/15))
- Read GeoParquet and Arrow, and tile DuckDB queries and data frames from Python ([#13](https://github.com/birkskyum/manby/pull/13))

### Fixed

- Read GeoJSON with a third coordinate in linear time ([#11](https://github.com/birkskyum/manby/pull/11))

### Dependencies

- Bump rusqlite from 0.40.2 to 0.41.1 ([#6](https://github.com/birkskyum/manby/pull/6))
- Bump the cargo group with 2 updates ([#5](https://github.com/birkskyum/manby/pull/5))
- Bump tikv-jemallocator from 0.6.1 to 0.7.0 ([#7](https://github.com/birkskyum/manby/pull/7))

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
