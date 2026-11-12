# manby for R

manby builds vector tilesets from sf data frames, DuckDB tables, Arrow tables and files, with the tiles that [tippecanoe](https://github.com/felt/tippecanoe) makes from the same features, several times faster. It runs on macOS, Linux and Windows.

```r
install.packages(
  "manby",
  repos = c("https://birkskyum.r-universe.dev", "https://cloud.r-project.org")
)
```

```r
library(manby)

nc <- sf::st_read(system.file("shape/nc.shp", package = "sf"))
tile(nc, "nc.pmtiles", layer = "counties", maximum_zoom = "g", coalesce_densest_as_needed = TRUE)
```

The options are tippecanoe's long options. The package downloads the manby program once from the GitHub release of its own version. In webR, in the browser, manby is built into the package. [From R](https://birkskyum.github.io/manby/guides/r/) has more.
