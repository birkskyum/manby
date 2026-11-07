#' The manby program
#'
#' `manby_binary()` gives the path of the manby program that the package runs:
#' the one that the `manby.binary` option or the `MANBY_BINARY` environment
#' variable names, or else the one that `install_manby()` downloaded. If there
#' is neither, it calls `install_manby()`.
#'
#' `install_manby()` downloads manby from the GitHub release of the package's
#' version, checks it against the release's checksums, and keeps it in the
#' package's cache, [tools::R_user_dir()]. manby runs on macOS and on Linux.
#'
#' @param version The version of manby to download. The default is the
#'   package's own.
#' @param force Download it again, even if it is in the cache.
#' @return The path of the program.
#' @examples
#' \dontrun{
#' manby_binary()
#' }
#' @export
manby_binary <- function() {
  path <- getOption("manby.binary", Sys.getenv("MANBY_BINARY"))
  if (nzchar(path)) {
    if (!file.exists(path)) {
      stop("manby was not found at ", path, call. = FALSE)
    }
    return(path)
  }
  path <- cached(own_version())
  if (!file.exists(path)) {
    path <- install_manby()
  }
  path
}

#' @rdname manby_binary
#' @export
install_manby <- function(version = own_version(), force = FALSE) {
  version <- sub("^v", "", version)
  path <- cached(version)
  if (file.exists(path) && !force) {
    return(invisible(path))
  }
  dir <- tempfile("manby-")
  dir.create(dir)
  on.exit(unlink(dir, recursive = TRUE), add = TRUE)
  targets <- targets()
  for (i in seq_along(targets)) {
    message("Downloading manby ", version, " for ", targets[i])
    bin <- download(version, targets[i], file.path(dir, i))
    # the next target is the static build, for a C library too old for this one
    if (i == length(targets) || runs(bin)) {
      break
    }
  }
  dir.create(dirname(path), recursive = TRUE, showWarnings = FALSE)
  file.copy(bin, path, overwrite = TRUE)
  Sys.chmod(path, "0755")
  invisible(path)
}

own_version <- function() {
  format(utils::packageVersion("manby"))
}

cached <- function(version) {
  file.path(tools::R_user_dir("manby", "cache"), version, "manby")
}

#' The release builds to try for this computer, in order.
#' @noRd
targets <- function() {
  arch <- switch(R.version$arch,
    aarch64 = ,
    arm64 = "aarch64",
    x86_64 = ,
    amd64 = "x86_64",
    stop("manby is built for 64-bit ARM and x86 computers, not ", R.version$arch, call. = FALSE)
  )
  switch(Sys.info()[["sysname"]],
    Darwin = paste0(arch, "-apple-darwin"),
    Linux = if (length(Sys.glob("/lib/ld-musl-*"))) {
      paste0(arch, "-unknown-linux-musl")
    } else {
      paste0(arch, "-unknown-linux-", c("gnu", "musl"))
    },
    stop("manby runs on macOS and Linux. On Windows, run R in WSL.", call. = FALSE)
  )
}

#' Downloads and checks a release's build into `dir`, and gives the
#' program's path.
#' @noRd
download <- function(version, target, dir) {
  url <- sprintf("https://github.com/birkskyum/manby/releases/download/v%s/", version)
  name <- sprintf("manby-%s.tar.gz", target)
  dir.create(dir)
  timeout <- options(timeout = max(300, getOption("timeout")))
  on.exit(options(timeout), add = TRUE)
  fetch <- function(file) {
    status <- tryCatch(
      utils::download.file(paste0(url, file), file.path(dir, file), mode = "wb", quiet = TRUE),
      error = function(e) 1L,
      warning = function(w) 1L
    )
    if (status != 0) {
      stop(
        "manby ", version, " could not be downloaded from ", url, file,
        ". Install it another way, such as `pip install manby` or ",
        "`cargo install manby`, and set the MANBY_BINARY environment variable to its path.",
        call. = FALSE
      )
    }
    file.path(dir, file)
  }
  sums <- readLines(fetch("SHA256SUMS"))
  tarball <- fetch(name)
  expected <- sub(" .*", "", sums[endsWith(sums, paste0(" ", name))])
  if (length(expected) != 1 || !identical(tolower(expected), sha256(tarball))) {
    stop("the download of ", name, " does not match the release's checksum", call. = FALSE)
  }
  utils::untar(tarball, exdir = dir)
  file.path(dir, sub("\\.tar\\.gz$", "", name), "manby")
}

sha256 <- function(path) {
  tools <- asNamespace("tools")
  if (exists("sha256sum", envir = tools)) {
    return(unname(get("sha256sum", envir = tools)(path)))
  }
  out <- if (nzchar(Sys.which("sha256sum"))) {
    system2("sha256sum", shQuote(path), stdout = TRUE)
  } else {
    system2("shasum", c("-a", "256", shQuote(path)), stdout = TRUE)
  }
  tolower(sub(" .*", "", out[1]))
}

runs <- function(bin) {
  Sys.chmod(bin, "0755")
  status <- suppressWarnings(system2(bin, "--version", stdout = FALSE, stderr = FALSE))
  identical(status, 0L)
}
