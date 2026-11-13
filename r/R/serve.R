#' Serves tilesets with a map of them
#'
#' Serves PMTiles archives, MBTiles databases or directories of tiles with
#' `manby serve`, until `$stop()` or the end of the R session. The map, at
#' `$url`, shows each tileset's layers on a basemap, with the attributes of
#' the features clicked. A tileset made again while it is served shows when
#' the map is reloaded.
#'
#' Each tileset's tiles and TileJSON can also go in a map of one's own, as
#' can a PMTiles archive itself, at its file name after `$url`.
#'
#' @param tilesets The paths of the tilesets.
#' @param port The port, or 0 for any free port.
#' @param host The address to listen on.
#' @param open Show the map, in the viewer pane or a browser.
#' With manby built into the package, as from CRAN, it serves on threads of
#' R's own process, until `$stop()` or the server is garbage collected.
#'
#' @return A `manby_server`, with `url`, the map's address, `tilejson`, the
#'   TileJSON address of each tileset by name, `stop()`, which ends it, and
#'   `alive()`, whether it serves.
#' @examples
#' \dontrun{
#' server <- serve("roads.pmtiles")
#' server$tilejson[["roads"]]
#' server$stop()
#' }
#' @export
serve <- function(tilesets, port = 8080, host = "127.0.0.1", open = interactive()) {
  if (in_webr()) {
    not_in_webr("serve()")
  }
  args <- c(
    "serve", "--port", format(port, scientific = FALSE), "--host", host,
    path.expand(as.character(tilesets))
  )
  server <- if (built_in()) serve_built_in(args) else serve_program(args)
  if (isTRUE(open)) {
    getOption("viewer", utils::browseURL)(server$url)
  }
  server
}

#' A manby_server from the lines that manby serve writes: the map's address,
#' then each tileset's TileJSON's.
#' @noRd
manby_server <- function(lines, alive, stop) {
  pairs <- strsplit(lines[-1], ": ", fixed = TRUE)
  structure(
    list(
      url = lines[1],
      tilejson = stats::setNames(
        vapply(pairs, `[`, "", 2),
        vapply(pairs, `[`, "", 1)
      ),
      alive = alive,
      stop = stop
    ),
    class = "manby_server"
  )
}

#' Serves on threads of manby built into the package, until stopped or
#' garbage collected.
#' @noRd
serve_built_in <- function(args) {
  made <- builtin_serve(args[-1])
  if (made[[1]] != 0) {
    stop(manby_error(made[[1]], args))
  }
  handle <- made[[3]]
  running <- TRUE
  manby_server(
    strsplit(made[[2]], "\n")[[1]],
    alive = function() running,
    stop = function() {
      builtin_stop(handle)
      running <<- FALSE
      invisible()
    }
  )
}

#' Serves with the manby program, until stopped or the end of the R session.
#' @noRd
serve_program <- function(args) {
  err <- tempfile("manby-serve-", fileext = ".txt")
  p <- processx::process$new(manby_binary(), args, stdout = "|", stderr = err, cleanup = TRUE)
  # the map's address, then each tileset's TileJSON
  lines <- character()
  while (length(lines) <= length(tilesets) && p$is_alive()) {
    p$poll_io(1000)
    lines <- c(lines, p$read_output_lines())
  }
  lines <- c(lines, if (!p$is_alive()) p$read_output_lines())
  if (!length(lines)) {
    p$wait()
    stop(manby_error(p$get_exit_status(), args, if (file.exists(err)) readLines(err)))
  }
  manby_server(
    lines,
    alive = function() p$is_alive(),
    stop = function() {
      if (p$is_alive()) p$kill()
      invisible()
    }
  )
}

#' @export
print.manby_server <- function(x, ...) {
  cat(
    "manby serve", if (x$alive()) "at" else "stopped, was at", x$url, "\n",
    paste0("  ", names(x$tilejson), ": ", x$tilejson, "\n"),
    sep = ""
  )
  invisible(x)
}
