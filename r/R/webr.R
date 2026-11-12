#' Whether R runs in webR, in a browser or Node.js, where R can't run
#' programs, so manby runs in R's own process instead.
#' @noRd
in_webr <- function() {
  identical(R.version$os, "emscripten")
}

#' Runs manby's command line in webR and gives its exit status.
#' @noRd
webr_run <- function(args) {
  .Call("manby_run_", c("manby", args), PACKAGE = "manby")
}

#' `manby stats` in webR: its exit status and its reports.
#' @noRd
webr_stats <- function(args) {
  .Call("manby_stats_", c("manby stats", args), PACKAGE = "manby")
}

not_in_webr <- function(what) {
  stop(what, " does not run in webR, where R can't run programs.", call. = FALSE)
}

.onLoad <- function(libname, pkgname) {
  if (in_webr()) {
    library.dynam("manby", pkgname, libname)
  }
}
