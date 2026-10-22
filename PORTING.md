# Porting notes

tippecanoe-rs is a faithful port of [felt/tippecanoe](https://github.com/felt/tippecanoe) v2.82.0.
The C++ source it follows lives next to this repository in `../tippecanoe`.

The goal is the same commands, the same flags, the same messages and the same output.
Every golden file in `tests/` was made by the C++ build, and the Rust build has to reproduce it byte for byte.
Changes to algorithms, flags or output belong in a later project, not here.

## Layout

There is one Rust module per C++ source file, with the same name (`-` becomes `_`).
`main.cpp` is `src/tippecanoe_main.rs`, since `src/main.rs` would turn into a binary.
Each program in `src/bin/` only calls the `main` function of its module.

| C++ | Rust |
| --- | --- |
| `mapbox/geometry/wagyu` | `crates/wagyu` |
| `clipper2` | `crates/clipper2` |
| `jsonpull/jsonpull.cpp` | `src/jsonpull.rs` |
| `milo/dtoa_milo.h` | `src/milo.rs` |
| `pmtiles/pmtiles.hpp` | `src/pmtiles.rs` |
| `protozero` | `src/protozero.rs` (only what `mvt.cpp` uses) |
| `flatgeobuf/*_generated.h` | `src/flatgeobuf_generated.rs` |
| `getopt_long` from libc | `src/getopt.rs` |
| C library helpers such as `atof` | `src/cstd.rs` |
| globals from `main.hpp` and `options.hpp` | `src/globals.rs` |
| `errors.hpp` | `src/errors.rs` |
| `unit.cpp` test cases | `#[cfg(test)]` modules next to the code they test |

## Translation rules

Keep the structure of the C++. Functions keep their names, their order in the file and their comments.
A `static` function becomes a private `fn`. Someone reading both versions side by side should be able to follow along.

Faithfulness wins over idiom whenever the two disagree.
Use the same algorithm, the same loop order, the same constants and the same order of floating point operations.
Rust never fuses `a * b + c` into one instruction, so write the expression exactly as the C++ does.
`crates/clipper2` matches the C++ built with `-ffp-contract=off`. Apple clang on arm64 fuses by default, which changes Clipper2's output for a few percent of random inputs.
`crates/wagyu` does too. Fusing changes wagyu's output for about one random input in ten thousand (slivers with large coordinates) and for none of the fixture polygons.
Where wagyu's output depends on the C++ standard library, it follows libc++. `std::stable_sort` is ported from it, and entries under one key of an `unordered_multimap` keep their insertion order (libstdc++ puts the newest first).

### Numbers

| C++ | Rust |
| --- | --- |
| `int`, `unsigned` | `i32`, `u32` |
| `long long`, `unsigned long long` | `i64`, `u64` |
| `long`, `unsigned long` | `i64`, `u64` (LP64) |
| `size_t`, `ssize_t` | `usize`, `isize` |
| `signed char`, `char` used as a number | `i8` |
| `double`, `float` | `f64`, `f32` |

Unsigned arithmetic that can wrap in C++ (hashes, quadkeys, `x - 1` on zero) uses `wrapping_*`.
Signed overflow is undefined in C++, so treat any place where it could happen as a bug to note, not a behaviour to copy.
`(long long) d` truncates toward zero, and Rust's `d as i64` does the same for values in range.
Integer `/` and `%` behave the same in both languages.

C library parsing (`atof`, `atoi`, `atoll`, `strtod`, `strtoll`, `strtoull`) accepts a numeric prefix and ignores the rest.
Use the helpers in `src/cstd.rs` for those, never `str::parse`.

`printf` formats map to `format!` like this. `%f` is `{:.6}`, `%.1f` is `{:.1}`, `%lld` and `%llu` are `{}`.
Numbers written as JSON go through `milo::dtoa_milo`, exactly where the C++ calls it.

### Strings and bytes

`std::string` holding text becomes `String`, and `std::string` holding binary data (tiles, protobuf, compressed bytes) becomes `Vec<u8>`.
Input text is checked with `check_utf8` before it gets far, as in the C++.
`std::string` comparison is byte-wise, which matches `str` and `[u8]` ordering in Rust.

### Containers and sorting

| C++ | Rust |
| --- | --- |
| `std::vector` | `Vec` |
| `std::map`, `std::set` | `BTreeMap`, `BTreeSet` |
| `std::unordered_map`, `std::unordered_set` | `HashMap`, `HashSet` |
| `std::shared_ptr<T>` | `Arc<T>` (`Rc<T>` only if it never crosses a thread) |
| `std::sort`, `qsort`, `std::stable_sort` | `sort_by` with the same comparator |
| `std::priority_queue` | `BinaryHeap` with the same ordering |

Iteration order must never come from a `HashMap` if it can change the output.
If the C++ iterates an `unordered_map` in a way that reaches the output, write a `// PORT:` comment there and keep the iteration deterministic.
`sort_by` panics if a comparator is not a total order, so comparators on floats have to be written as the C++ intends them, not with `partial_cmp().unwrap()`.

### Errors and exit codes

The C++ prints a message and calls `exit()` deep inside the code.
Keep that. Print the same message to stderr with `eprint!` and call `std::process::exit` with the constant from `errors.rs`.
`perror(s)` is `crate::cstd::perror(s)`, which prints `s: <strerror(errno)>` like libc does.

### Threads, files and memory maps

`pthread_create` becomes `std::thread::scope` or `std::thread::spawn` with the same work split, so the output does not depend on scheduling.
Temporary files follow the C++ pattern (create in the temp dir, unlink at once, keep the descriptor).
Memory maps use `memmap2`. `FILE *` becomes `std::fs::File` behind a `BufReader` or `BufWriter`.
Raw `libc` calls are fine where Rust has no equivalent (`getrlimit`, `sysconf`, `fork` for filters). Mark each `unsafe` block with a `// SAFETY:` comment.

### Globals

Globals from `main.cpp` and `tile-join.cpp` live in `src/globals.rs`.
Scalars are atomics read with `Relaxed` ordering. Larger values sit in a `RwLock` and are set once while the options are parsed.

## Testing

`make test` runs the C++ test recipes against the Rust binaries.
Unit tests from `unit.cpp` run with `cargo test`.
To compare against the C++ build directly, build `../tippecanoe` with `make` and run both programs on the same input.
