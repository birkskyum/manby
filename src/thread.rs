//! Port of thread.cpp.
//!
//! It is harder to profile tippecanoe because of its normal use of multiple threads.
//! If you set TIPPECANOE_NO_THREADS in the environment, it will run everything in
//! the main thread instead for more straightforward profiling. (The threads will
//! then still be created so they will be reaped, but they will immediately return.)

use std::sync::LazyLock;
use std::thread::{Scope, ScopedJoinHandle};

static NO_THREADS: LazyLock<bool> =
    LazyLock::new(|| std::env::var_os("TIPPECANOE_NO_THREADS").is_some());

/// Port of `thread_create`, for threads spawned inside `std::thread::scope`.
pub fn thread_create<'scope, 'env, F, T>(
    scope: &'scope Scope<'scope, 'env>,
    f: F,
) -> ScopedJoinHandle<'scope, T>
where
    F: FnOnce() -> T + Send + 'scope,
    T: Send + 'scope,
{
    if *NO_THREADS {
        let ret = f();
        scope.spawn(move || ret)
    } else {
        scope.spawn(f)
    }
}
