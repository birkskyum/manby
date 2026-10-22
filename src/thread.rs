//! Port of thread.cpp.
//!
//! It is harder to profile tippecanoe because of its normal use of multiple threads.
//! If you set TIPPECANOE_NO_THREADS in the environment, it will run everything in
//! the main thread instead for more straightforward profiling. (The threads will
//! then still be created so they will be reaped, but they will immediately return.)

use std::sync::LazyLock;
use std::thread::{Scope, ScopedJoinHandle};

use crate::cstd::perror_io;
use crate::errors::EXIT_PTHREAD;

static NO_THREADS: LazyLock<bool> =
    LazyLock::new(|| std::env::var_os("TIPPECANOE_NO_THREADS").is_some());

/// Port of `thread_create`, for threads spawned inside `std::thread::scope`.
///
/// Every C++ caller follows a failed `thread_create` with
/// `perror("pthread_create")` and `exit(EXIT_PTHREAD)`, so that happens here.
pub fn thread_create<'scope, 'env, F, T>(
    scope: &'scope Scope<'scope, 'env>,
    f: F,
) -> ScopedJoinHandle<'scope, T>
where
    F: FnOnce() -> T + Send + 'scope,
    T: Send + 'scope,
{
    let spawned = if *NO_THREADS {
        let ret = f();
        std::thread::Builder::new().spawn_scoped(scope, move || ret)
    } else {
        std::thread::Builder::new().spawn_scoped(scope, f)
    };

    match spawned {
        Ok(handle) => handle,
        Err(e) => {
            perror_io("pthread_create", &e);
            std::process::exit(EXIT_PTHREAD);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_and_joins() {
        let mut out = [0; 4];
        std::thread::scope(|scope| {
            let handles: Vec<_> = out
                .iter_mut()
                .enumerate()
                .map(|(i, o)| {
                    thread_create(scope, move || {
                        *o = i * 2;
                        i
                    })
                })
                .collect();
            let joined: Vec<usize> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            assert_eq!(joined, vec![0, 1, 2, 3]);
        });
        assert_eq!(out, [0, 2, 4, 6]);
    }
}
