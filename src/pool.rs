//! Thread pools. WebAssembly as webR runs it has no threads, so there a
//! pool is the calling thread, and rayon's work runs on it in turn.

use crate::Error;
use tippecanoe::errors::EXIT_PTHREAD;

/// How many threads to work on: `requested`, or one per core for 0. Always
/// one on WebAssembly.
pub(crate) fn threads(requested: usize) -> usize {
    if cfg!(target_family = "wasm") {
        1
    } else if requested > 0 {
        requested
    } else {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    }
}

/// A rayon pool of some threads, or on WebAssembly the calling thread.
pub(crate) struct Pool {
    #[cfg(not(target_family = "wasm"))]
    pool: rayon::ThreadPool,
}

impl Pool {
    pub(crate) fn new(threads: usize) -> crate::Result<Pool> {
        #[cfg(not(target_family = "wasm"))]
        {
            rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .map(|pool| Pool { pool })
                .map_err(|e| Error::new(EXIT_PTHREAD, e.to_string()))
        }
        #[cfg(target_family = "wasm")]
        {
            let _ = threads;
            // rayon's own pool would start threads, which fails, so its
            // global pool is made of this thread once.
            static ONCE: std::sync::Once = std::sync::Once::new();
            let mut failed = None;
            ONCE.call_once(|| {
                failed = rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .use_current_thread()
                    .build_global()
                    .err();
            });
            match failed {
                Some(e) => Err(Error::new(EXIT_PTHREAD, e.to_string())),
                None => Ok(Pool {}),
            }
        }
    }

    /// Runs `f` with the pool as rayon's.
    pub(crate) fn install<R: Send>(&self, f: impl FnOnce() -> R + Send) -> R {
        #[cfg(not(target_family = "wasm"))]
        {
            self.pool.install(f)
        }
        #[cfg(target_family = "wasm")]
        {
            f()
        }
    }

    /// rayon's `in_place_scope`, in the pool.
    pub(crate) fn in_place_scope<'scope, R>(
        &self,
        f: impl FnOnce(&rayon::Scope<'scope>) -> R,
    ) -> R {
        #[cfg(not(target_family = "wasm"))]
        {
            self.pool.in_place_scope(f)
        }
        #[cfg(target_family = "wasm")]
        {
            rayon::in_place_scope(f)
        }
    }
}
