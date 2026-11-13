## Submission

manby builds vector tilesets in PMTiles and MBTiles, with the tiles of
tippecanoe, from sf data frames, DuckDB and Arrow tables and files.

## Rust

The package builds manby, a Rust port of tippecanoe, from its source and the
crates it needs, vendored in src/rust/vendor.tar.xz, offline, with two jobs
(cargo build --offline --locked -j 2), and prints the versions of cargo and
rustc in the installation log. It builds with rustc 1.90 and later. The
crates' authors and licenses are listed in inst/AUTHORS. The build directory
and the unpacked crates are removed once the library is linked.

## Note

The source package is about 11 MB, as it carries the vendored crates so that
CRAN builds it offline. Crates that none of CRAN's platforms build are left
as their manifests, and the crates' tests and benchmarks are left out.

## Test environments

* macOS, arm64, R 4.3, rustc 1.98.1: R CMD check --as-cran.
* GitHub Actions: Ubuntu 24.04, macOS and Windows Server 2025, with rustc
  1.91.1, the oldest of CRAN's machines: R CMD check --as-cran.
