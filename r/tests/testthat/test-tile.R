# The same features from sf, a data frame, nanoarrow, Arrow and DuckDB make
# the tiles that manby makes from GDAL's FlatGeobuf of them, as tippecanoe.
# In webR, manby is built into the package, and the programs are left out.

webr <- manby:::in_webr()
skip_if(!webr && inherits(try(manby_binary(), silent = TRUE), "try-error"), "manby is not available")
skip_if_not_installed("sf")

# A PMTiles archive's tile data and layers.
tiles <- function(path) {
  con <- file(path, "rb")
  on.exit(close(con))
  h <- readBin(con, "integer", n = 32, size = 4)
  at <- function(i) h[i + 1] + h[i + 2] * 2^32
  seek(con, at(6))
  metadata <- readBin(con, "raw", n = at(8))
  seek(con, at(14))
  data <- readBin(con, "raw", n = at(16))
  json <- rawToChar(memDecompress(metadata, type = "gzip"))
  list(data = data, layers = jsonlite::fromJSON(json)$vector_layers$id)
}

n <- 20000
i <- seq_len(n) - 1
points <- sf::st_as_sf(
  data.frame(
    n = as.integer(i),
    kind = paste0("k", i %% 13),
    score = i / 7,
    x = -10 + 40 * ((i * 7919) %% n) / n,
    y = 35 + 25 * ((i * 104729) %% n) / n
  ),
  coords = c("x", "y"),
  crs = 4326
)
wkb <- sf::st_drop_geometry(points)
wkb$geometry <- unclass(sf::st_as_binary(sf::st_geometry(points)))

dir <- withr::local_tempdir()
fgb <- file.path(dir, "points.fgb")
sf::st_write(points, fgb, layer_options = "SPATIAL_INDEX=NO", quiet = TRUE)

make <- function(data, name, ...) {
  out <- file.path(dir, name, "points.pmtiles")
  dir.create(dirname(out))
  expect_equal(tile(data, out, maximum_zoom = "g", drop_densest_as_needed = TRUE, quiet = TRUE, ...), out)
  tiles(out)
}

# named as the tables' layers are, after the tileset
reference <- make(fgb, "fgb", layer = "points")

test_that("an sf data frame makes tippecanoe's tiles", {
  t <- make(points, "sf")
  expect_equal(t$layers, "points")
  expect_identical(t$data, reference$data)
})

test_that("a data frame with well-known binary makes the same tiles", {
  expect_identical(make(wkb, "wkb")$data, reference$data)
  expect_identical(make(nanoarrow::as_nanoarrow_array_stream(wkb), "nanoarrow")$data, reference$data)
})

test_that("an Arrow table makes the same tiles", {
  skip_if_not_installed("arrow")
  expect_identical(make(arrow::as_arrow_table(wkb), "arrow")$data, reference$data)
})

test_that("a DuckDB table makes the same tiles", {
  skip_if_not_installed("duckdb")
  skip_if_not_installed("dbplyr")
  con <- DBI::dbConnect(duckdb::duckdb())
  on.exit(DBI::dbDisconnect(con, shutdown = TRUE))
  duckdb::duckdb_register(con, "points", wkb)
  expect_identical(make(dplyr::tbl(con, "points"), "duckdb")$data, reference$data)
  query <- dplyr::tbl(con, dplyr::sql("SELECT * FROM points WHERE kind = 'k3'"))
  expect_true(length(make(query, "query")$data) > 0)
})

test_that("an sf data frame in another CRS is reprojected", {
  t <- make(sf::st_transform(points, 3035), "projected")
  expect_equal(t$layers, "points")
  expect_true(length(t$data) > 0)
})

test_that("a file that GDAL reads makes the same tiles, through sf and DuckDB", {
  # GDAL's SQLite driver fails in webR's sf
  skip_if(webr, "webR")
  sqlite <- file.path(dir, "points.sqlite")
  sf::st_write(points, sqlite, quiet = TRUE)
  expect_true(is_gdal(sqlite))
  expect_false(is_gdal(fgb))
  t <- make(sqlite, "sqlite")
  expect_equal(t$layers, "points")
  expect_identical(t$data, reference$data)

  skip_if_not_installed("duckdb")
  skip_on_cran()
  projected <- file.path(dir, "projected.sqlite")
  sf::st_write(sf::st_transform(points, 3035), projected, quiet = TRUE)
  out <- file.path(dir, "duckdb-gdal")
  dir.create(out)
  file <- write_gdal(sqlite, "", file.path(out, "points"), sf = FALSE)
  expect_identical(make(file, "duckdb-sqlite", layer = "points")$data, reference$data)
  # reprojected there and back, so near but not the same
  file <- write_gdal(projected, "", file.path(out, "projected"), sf = FALSE)
  expect_true(length(make(file, "duckdb-projected", layer = "points")$data) > 0)
})

test_that("h3_sql gives the hexagons DuckDB's aggregates", {
  skip_if(webr, "webR")
  skip_if_not_installed("duckdb")
  skip_on_cran()
  fields <- function(path) {
    con <- file(path, "rb")
    on.exit(close(con))
    h <- readBin(con, "integer", n = 32, size = 4)
    seek(con, h[7] + h[8] * 2^32)
    json <- rawToChar(memDecompress(readBin(con, "raw", n = h[9] + h[10] * 2^32), type = "gzip"))
    layers <- jsonlite::fromJSON(json, simplifyDataFrame = FALSE)$vector_layers
    names(layers[[which(endsWith(vapply(layers, `[[`, "", "id"), "_h3"))]]$fields)
  }
  sql <- c(mean_score = "avg(score)", kinds = "count(DISTINCT kind)")
  out <- file.path(dir, "h3sql.pmtiles")
  tile(points, out, maximum_zoom = 6, h3_no_points = TRUE, h3_sql = sql, quiet = TRUE)
  expect_true(all(c("mean_score", "kinds", "point_count") %in% fields(out)))

  sqlite <- file.path(dir, "h3sql.sqlite")
  sf::st_write(points, sqlite, quiet = TRUE)
  out <- file.path(dir, "h3sql-gdal.pmtiles")
  tile(sqlite, out, layer = "points", maximum_zoom = 6, h3_no_points = TRUE, h3_sql = sql, quiet = TRUE)
  expect_true(all(c("mean_score", "kinds") %in% fields(out)))
  expect_error(tile(points, out, h3 = TRUE, h3_sql = "avg(score)"), "needs a name")
})

test_that("a list makes layers", {
  out <- file.path(dir, "layers.pmtiles")
  tile(list(a = points, b = fgb), out, maximum_zoom = 4, quiet = TRUE)
  expect_equal(tiles(out)$layers, c("a", "b"))
  expect_error(tile(list(points), out), "needs a name")
})

test_that("an error from manby is a manby_error", {
  e <- tryCatch(
    tile(points, file.path(dir, "bad.pmtiles"), maximum_zoom = "x", quiet = TRUE),
    manby_error = function(e) e
  )
  expect_s3_class(e, "manby_error")
  expect_true(e$status != 0)
})

test_that("options become tippecanoe's long options", {
  expect_equal(
    flags(list(maximum_zoom = 14, drop_densest_as_needed = TRUE, quiet = FALSE, include = c("a", "b"), x = NULL, r = 2.5, n = 1e6)),
    c("--maximum-zoom=14", "--drop-densest-as-needed", "--include=a", "--include=b", "--r=2.5", "--n=1000000")
  )
})

test_that("serve serves a tileset and its map", {
  skip_if(webr, "webR can't run programs")
  server <- serve(file.path(dir, "fgb", "points.pmtiles"), port = 0, open = FALSE)
  on.exit(server$stop())
  expect_s3_class(server, "manby_server")
  expect_equal(names(server$tilejson), "points")
  expect_equal(jsonlite::fromJSON(server$tilejson[["points"]])$vector_layers$id, "points")
  expect_true(any(grepl("maplibre-gl", readLines(server$url, warn = FALSE))))
  server$stop()
  expect_false(server$process$is_alive())
  expect_error(serve(file.path(dir, "nothing.pmtiles"), port = 0, open = FALSE), class = "manby_error")
})

test_that("tile_stats reports the tiles' sizes and features", {
  s <- tile_stats(file.path(dir, "fgb", "points.pmtiles"), layers = TRUE, largest = 3)
  expect_true(sum(s$zooms$tiles) > 0)
  expect_equal(nrow(s$largest), 3)
  expect_equal(unique(s$layers$layer), "points")
  expect_true(all(s$layers$features > 0))
  expect_equal(sum(s$layers$tiles), sum(s$zooms$tiles))
})

test_that("serve and the program are not in webR", {
  skip_if_not(webr)
  expect_error(serve(file.path(dir, "fgb", "points.pmtiles")), "does not run in webR")
  expect_error(manby_binary(), "does not run in webR")
})

test_that("manby runs", {
  # in webR, manby writes to the console itself
  skip_if(webr, "manby's messages don't go through R in webR")
  expect_match(paste(capture.output(manby("--version"), type = "message"), collapse = ""), "^manby ")
})

test_that("manby runs in webR", {
  skip_if_not(webr)
  expect_no_error(manby("--version"))
  expect_error(manby("--nothing-like-this"), class = "manby_error")
})
