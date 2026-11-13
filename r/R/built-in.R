#' Whether R runs in webR, in a browser or Node.js, where R can't run
#' programs.
#' @noRd
in_webr <- function() {
  identical(R.version$os, "emscripten")
}

# whether manby is built into the package, found when it is loaded
state <- new.env(parent = emptyenv())
state$built_in <- FALSE

#' Whether manby is built into the package and runs in R's own process: in
#' webR, and from CRAN, whose source package has the crates to build it.
#' Elsewhere the package runs the manby program.
#' @noRd
built_in <- function() {
  state$built_in
}

#' Runs manby's command line in R's process and gives its exit status.
#' @noRd
builtin_run <- function(args) {
  .Call("manby_run_", c("manby", args), PACKAGE = "manby")
}

#' `manby stats` in R's process: its exit status and its reports.
#' @noRd
builtin_stats <- function(args) {
  .Call("manby_stats_", c("manby stats", args), PACKAGE = "manby")
}

#' `manby serve` on threads in R's process: its exit status, the addresses,
#' and the server, for `builtin_stop()`.
#' @noRd
builtin_serve <- function(args) {
  .Call("manby_serve_", c("manby serve", args), PACKAGE = "manby")
}

builtin_stop <- function(server) {
  .Call("manby_serve_stop_", server, PACKAGE = "manby")
}

not_in_webr <- function(what) {
  stop(what, " does not run in webR, where R can't run programs.", call. = FALSE)
}

.onLoad <- function(libname, pkgname) {
  # the package's library, which only a package with manby built in has
  state$built_in <- file.exists(file.path(
    libname, pkgname, "libs", .Platform$r_arch, paste0(pkgname, .Platform$dynlib.ext)
  ))
  if (state$built_in) {
    library.dynam("manby", pkgname, libname)
  }
}
