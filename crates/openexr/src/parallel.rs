// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Threads: the C++ library's `IlmThread` pool, as `rayon` (PLAN.md O12). Single-threaded by
//! default; on request, a read decodes its chunks, and a write compresses its chunks, on
//! several threads, the results the same bytes as single-threaded. Without the `rayon`
//! feature every request runs single-threaded.

use crate::error::Result;

/// How many threads a file's reads or writes use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Threads {
    /// One: the calling thread (the default).
    #[default]
    Serial,
    /// The current `rayon` pool: the global one, or the application's own when called from
    /// inside its `install`.
    Auto,
    /// A pool of this many threads of the file's own (0 and 1 are one: the calling thread).
    Count(usize),
}

/// The threads a file runs its chunks on.
#[derive(Clone, Default)]
pub(crate) enum Pool {
    #[default]
    Serial,
    #[cfg(feature = "rayon")]
    Current,
    #[cfg(feature = "rayon")]
    Own(std::sync::Arc<rayon::ThreadPool>),
}

impl std::fmt::Debug for Pool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Pool::Serial => f.write_str("Serial"),
            #[cfg(feature = "rayon")]
            Pool::Current => f.write_str("Current"),
            #[cfg(feature = "rayon")]
            Pool::Own(p) => write!(f, "Own({})", p.current_num_threads()),
        }
    }
}

impl Pool {
    pub(crate) fn new(threads: Threads) -> Result<Pool> {
        match threads {
            Threads::Serial | Threads::Count(0) | Threads::Count(1) => Ok(Pool::Serial),
            #[cfg(feature = "rayon")]
            Threads::Auto => Ok(Pool::Current),
            #[cfg(feature = "rayon")]
            Threads::Count(n) => rayon::ThreadPoolBuilder::new()
                .num_threads(n)
                .build()
                .map(|p| Pool::Own(std::sync::Arc::new(p)))
                .map_err(|e| {
                    crate::error::Error::new(
                        crate::error::ErrorCode::OutOfMemory,
                        format!("Unable to start {n} threads: {e}"),
                    )
                }),
            #[cfg(not(feature = "rayon"))]
            _ => Ok(Pool::Serial),
        }
    }

    /// Whether the pool runs work on more than the calling thread.
    pub(crate) fn is_parallel(&self) -> bool {
        !matches!(self, Pool::Serial)
    }

    /// `f` of each item, in the items' order, on the pool's threads.
    pub(crate) fn map<I: Send, O: Send>(
        &self,
        items: Vec<I>,
        f: impl Fn(I) -> O + Sync + Send,
    ) -> Vec<O> {
        match self {
            Pool::Serial => items.into_iter().map(f).collect(),
            #[cfg(feature = "rayon")]
            Pool::Current => {
                use rayon::prelude::*;
                items.into_par_iter().map(f).collect()
            }
            #[cfg(feature = "rayon")]
            Pool::Own(p) => p.install(|| {
                use rayon::prelude::*;
                items.into_par_iter().map(f).collect()
            }),
        }
    }
}
