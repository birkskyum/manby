# Contributing

Pull requests are welcome. A pull request's title becomes its commit on main and its line in the [changelog](CHANGELOG.md), so it starts with a type from [Conventional Commits](https://www.conventionalcommits.org), such as `feat:` for something new, `fix:` for a bug, `perf:` for speed or memory, or `docs:`, `test:`, `ci:` and `chore:` for the rest. The title can be fixed when merging.

CI checks the formatting, lints manby's code with clippy, and runs the unit tests and tippecanoe's golden tests, as MVT and as MLT, on Linux and macOS. Locally that is `cargo fmt --check`, `cargo clippy --features mlt,remote -p manby --all-targets --no-deps -- -D warnings` and `make test test-golden-mlt`.

Releases need no one. Each merge updates the release PR, and merging that publishes manby to crates.io, PyPI, the GitHub releases and Homebrew.
