#' Makes a tileset
#'
#' Makes the tileset `output` from `data`, with the tiles that tippecanoe makes
#' from the same features.
#'
#' `data` is a file that manby reads, such as GeoJSON, FlatGeobuf, CSV,
#' GeoParquet, GeoPackage or Shapefile, or a table:
#'
#' - An sf data frame. One in a CRS other than WGS 84 or Web Mercator is
#'   reprojected to WGS 84 first.
#' - A DuckDB table from dplyr, such as `dplyr::tbl(con, "roads")` or
#'   `dplyr::tbl(con, dplyr::sql("SELECT ..."))`, which DuckDB writes to a
#'   temporary Parquet file, so it need not fit in memory.
#' - An Arrow table or stream, or a data frame, with its geometry in a GeoArrow
#'   column or in a column named `geometry` in well-known binary.
#'
#' A named list makes a layer of each of these. Other tables are written to a
#' temporary Arrow file for manby to read.
#'
#' @param data The features: a file name, a table, or a named list of layers.
#' @param output The tileset to write, such as `"roads.pmtiles"`.
#' @param ... tippecanoe's options. A named argument is a long option, so
#'   `maximum_zoom = 14` is `--maximum-zoom=14`, `drop_densest_as_needed =
#'   TRUE` is `--drop-densest-as-needed`, and a vector repeats an option. Other
#'   arguments are passed as they are, such as `"-zg"`.
#' @param layer The name of the layer, for a single input. Without one, the
#'   layer is named after the file, or for a table after the tileset.
#' @param force Overwrite an existing tileset.
#' @return `output`, invisibly.
#' @examples
#' \dontrun{
#' nc <- sf::st_read(system.file("shape/nc.shp", package = "sf"))
#' tile(nc, "nc.pmtiles", layer = "counties", maximum_zoom = "g")
#'
#' con <- DBI::dbConnect(duckdb::duckdb())
#' roads <- dplyr::tbl(con, dplyr::sql(
#'   "SELECT name, class, geometry FROM 'roads.parquet' WHERE class = 'motorway'"
#' ))
#' tile(roads, "roads.pmtiles", maximum_zoom = 14, drop_densest_as_needed = TRUE)
#'
#' tile(list(roads = roads, places = "places.parquet"), "map.pmtiles", "-zg")
#' }
#' @export
tile <- function(data, output, ..., layer = NULL, force = TRUE) {
  output <- path.expand(as.character(output))
  if (is_layers(data)) {
    if (is.null(names(data)) || !all(nzchar(names(data)))) {
      stop("each layer in the list needs a name", call. = FALSE)
    }
    layers <- data
  } else {
    layers <- list(data)
    names(layers) <- if (is.null(layer)) "" else layer
  }
  dots <- list(...)
  named <- if (is.null(names(dots))) logical(length(dots)) else nzchar(names(dots))
  argv <- c(
    "-o", output,
    if (isTRUE(force)) "--force",
    flags(dots[named]),
    as.character(unlist(dots[!named]))
  )

  tmp <- NULL
  on.exit(if (!is.null(tmp)) unlink(tmp, recursive = TRUE), add = TRUE)
  # named as the tileset, which an unnamed layer is named after
  stem <- sub("\\.[^.]*$", "", basename(output))
  if (!nzchar(stem)) {
    stem <- "layer"
  }
  for (i in seq_along(layers)) {
    d <- layers[[i]]
    if (is_path(d)) {
      file <- path.expand(as.character(d))
    } else {
      if (is.null(tmp)) {
        tmp <- tempfile("manby-")
        dir.create(tmp)
      }
      dir <- file.path(tmp, i)
      dir.create(dir)
      file <- write_table(d, file.path(dir, stem))
    }
    name <- names(layers)[i]
    argv <- c(argv, if (nzchar(name)) {
      paste0("-L{\"file\":", json_string(file), ",\"layer\":", json_string(name), "}")
    } else {
      file
    })
  }
  run(argv)
  invisible(output)
}

#' Runs manby
#'
#' Runs manby with arguments as on the command line, such as `manby join`.
#'
#' @param ... The arguments.
#' @return Nothing. If manby fails, an error of class `manby_error`, with its
#'   exit status in `status`: 100 when the tileset is complete only to a lower
#'   zoom, as for tippecanoe.
#' @examples
#' \dontrun{
#' manby("join", "-o", "map.pmtiles", "roads.pmtiles", "places.pmtiles")
#' }
#' @export
manby <- function(...) {
  run(as.character(c(...)))
}

run <- function(args) {
  if (in_webr()) {
    status <- webr_run(args)
    if (status != 0) {
      stop(manby_error(status, args))
    }
    return(invisible())
  }
  out <- function(x, proc) cat(x)
  err <- function(x, proc) cat(x, file = stderr())
  status <- processx::run(
    manby_binary(), args,
    error_on_status = FALSE,
    stdout_callback = out,
    stderr_callback = err
  )$status
  if (status != 0) {
    stop(manby_error(status, args))
  }
  invisible()
}

manby_error <- function(status, args, detail = character()) {
  structure(
    class = c("manby_error", "error", "condition"),
    list(
      message = paste(
        c(paste0("manby exited with status ", status, ": ", paste(args, collapse = " ")), detail),
        collapse = "\n"
      ),
      call = NULL,
      status = status
    )
  )
}

flags <- function(options) {
  out <- character()
  for (key in names(options)) {
    flag <- paste0("--", gsub("_", "-", key, fixed = TRUE))
    for (v in as.list(options[[key]])) {
      if (is.null(v) || is.na(v) || isFALSE(v)) {
        next
      }
      out <- c(out, if (isTRUE(v)) {
        flag
      } else if (is.numeric(v)) {
        paste0(flag, "=", format(v, scientific = FALSE, digits = 15, trim = TRUE))
      } else {
        paste0(flag, "=", v)
      })
    }
  }
  out
}

is_layers <- function(x) {
  is.list(x) && !is.object(x)
}

is_path <- function(x) {
  (is.character(x) && length(x) == 1) || inherits(x, "fs_path")
}

json_string <- function(x) {
  x <- enc2utf8(as.character(x))
  for (r in list(c("\\", "\\\\"), c("\"", "\\\""), c("\n", "\\n"), c("\r", "\\r"), c("\t", "\\t"))) {
    x <- gsub(r[1], r[2], x, fixed = TRUE)
  }
  paste0("\"", x, "\"")
}

#' Writes a table for manby to read, as `base` with an extension, and gives
#' the file's name.
#' @noRd
write_table <- function(d, base) {
  if (inherits(d, "tbl_lazy")) {
    con <- dbplyr::remote_con(d)
    if (!inherits(con, "duckdb_connection")) {
      stop(
        "manby::tile reads tables from DuckDB. Collect this one first, ",
        "as an sf data frame for example.",
        call. = FALSE
      )
    }
    file <- paste0(base, ".parquet")
    DBI::dbExecute(con, paste0(
      "COPY (", dbplyr::sql_render(d), ") TO ",
      DBI::dbQuoteString(con, file), " (FORMAT parquet)"
    ))
    return(file)
  }
  file <- paste0(base, ".arrows")
  nanoarrow::write_nanoarrow(arrow_stream(d), file)
  file
}

arrow_stream <- function(d) {
  if (inherits(d, "sf") || (is.data.frame(d) && any(vapply(d, inherits, NA, "sfc")))) {
    return(sf_stream(d))
  }
  if (is.data.frame(d)) {
    return(frame_stream(d))
  }
  if (inherits(d, "arrow_dplyr_query")) {
    d <- arrow::as_record_batch_reader(d)
  }
  tryCatch(nanoarrow::as_nanoarrow_array_stream(d), error = function(e) {
    stop(
      "manby::tile takes a file name, an sf data frame, a DuckDB table, ",
      "or a table with an Arrow stream, such as an Arrow table, not ",
      class(d)[1],
      call. = FALSE
    )
  })
}

#' An sf data frame as a stream with its geometry in well-known binary, which
#' its GeoParquet metadata names, in WGS 84 or Web Mercator.
#' @noRd
sf_stream <- function(x) {
  if (!inherits(x, "sf")) {
    x <- sf::st_as_sf(x)
  }
  crs <- sf::st_crs(x)
  if (!is.na(crs) && !isTRUE(crs$epsg %in% c(4326, 3857))) {
    x <- sf::st_transform(x, 4326)
    crs <- sf::st_crs(4326)
  }
  name <- attr(x, "sf_column")
  geometry <- sf::st_geometry(x)
  df <- sf::st_drop_geometry(x)
  df <- df[!vapply(df, inherits, NA, "sfc")]
  df[[name]] <- unclass(sf::st_as_binary(geometry))
  crs <- if (is.na(crs)) {
    ",\"crs\":null"
  } else if (isTRUE(crs$epsg == 3857)) {
    ",\"crs\":\"EPSG:3857\""
  } else {
    "" # OGC:CRS84
  }
  frame_stream(df, binary = name, geo = paste0(
    "{\"version\":\"1.1.0\",\"primary_column\":", json_string(name),
    ",\"columns\":{", json_string(name),
    ":{\"encoding\":\"WKB\",\"geometry_types\":[]", crs, "}}}"
  ))
}

#' A data frame as a stream, in batches, with the column `binary` as binary
#' even if it is empty, and with GeoParquet metadata `geo`.
#' @noRd
frame_stream <- function(df, binary = NULL, geo = NULL) {
  df <- as.data.frame(df)
  for (name in names(df)) {
    df[[name]] <- column(df[[name]], name)
  }
  types <- lapply(df, nanoarrow::infer_nanoarrow_schema)
  types[binary] <- list(nanoarrow::na_binary())
  schema <- nanoarrow::na_struct(types)
  if (!is.null(geo)) {
    schema <- nanoarrow::nanoarrow_schema_modify(schema, list(metadata = list(geo = geo)))
  }
  n <- nrow(df)
  size <- 65536
  starts <- if (n > 0) seq(1, n, by = size) else integer()
  batches <- lapply(starts, function(s) {
    nanoarrow::as_nanoarrow_array(df[s:min(n, s + size - 1), , drop = FALSE], schema = schema)
  })
  nanoarrow::basic_array_stream(batches, schema = schema)
}

#' A column as nanoarrow writes it: factors as text, quantities as numbers,
#' and other values that Arrow has no type for as text. Lists other than of
#' well-known binary are left out.
#' @noRd
column <- function(x, name) {
  if (inherits(x, "wk_wkb")) {
    return(lapply(unclass(x), identity))
  }
  if (is.factor(x)) {
    return(as.character(x))
  }
  if (inherits(x, "POSIXlt")) {
    x <- as.POSIXct(x)
  }
  if (inherits(x, c("units", "difftime"))) {
    return(as.vector(unclass(x)))
  }
  if (!is.null(tryCatch(nanoarrow::infer_nanoarrow_schema(x), error = function(e) NULL))) {
    return(x)
  }
  if (is.list(x)) {
    warning("manby leaves out the column ", name, ", a list", call. = FALSE)
    return(NULL)
  }
  as.character(x)
}
