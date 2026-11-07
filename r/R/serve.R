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
#' @return A `manby_server`, with `url`, the map's address, `tilejson`, the
#'   TileJSON address of each tileset by name, and `stop()`, which ends it.
#' @examples
#' \dontrun{
#' server <- serve("roads.pmtiles")
#' server$tilejson[["roads"]]
#' server$stop()
#' }
#' @export
serve <- function(tilesets, port = 8080, host = "127.0.0.1", open = interactive()) {
  err <- tempfile("manby-serve-", fileext = ".txt")
  args <- c(
    "serve", "--port", format(port, scientific = FALSE), "--host", host,
    path.expand(as.character(tilesets))
  )
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
  pairs <- strsplit(lines[-1], ": ", fixed = TRUE)
  server <- structure(
    list(
      url = lines[1],
      tilejson = stats::setNames(
        vapply(pairs, `[`, "", 2),
        vapply(pairs, `[`, "", 1)
      ),
      process = p,
      stop = function() {
        if (p$is_alive()) p$kill()
        invisible()
      }
    ),
    class = "manby_server"
  )
  if (isTRUE(open)) {
    getOption("viewer", utils::browseURL)(server$url)
  }
  server
}

#' @export
print.manby_server <- function(x, ...) {
  cat(
    "manby serve", if (x$process$is_alive()) "at" else "stopped, was at", x$url, "\n",
    paste0("  ", names(x$tilejson), ": ", x$tilejson, "\n"),
    sep = ""
  )
  invisible(x)
}
