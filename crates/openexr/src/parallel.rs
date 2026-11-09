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

    /// `f` of each item, in the items' order, on the pool's threads: the items in about four
    /// runs a thread, each run one after another with one state `init` makes (a decoder or
    /// encoder whose buffers then serve the run's chunks).
    pub(crate) fn map_init<I: Send, O: Send, S>(
        &self,
        items: Vec<I>,
        init: impl Fn() -> S + Sync + Send,
        f: impl Fn(&mut S, I) -> O + Sync + Send,
    ) -> Vec<O> {
        let run_all = |items: Vec<I>| {
            let mut state = init();
            items
                .into_iter()
                .map(|i| f(&mut state, i))
                .collect::<Vec<O>>()
        };
        match self {
            Pool::Serial => run_all(items),
            #[cfg(feature = "rayon")]
            Pool::Current => runs(items, rayon::current_num_threads(), &run_all),
            #[cfg(feature = "rayon")]
            Pool::Own(p) => p.install(|| runs(items, p.current_num_threads(), &run_all)),
        }
    }
}

/// `items` cut into about four runs for each of `threads` threads, each run done by `run`
/// on the current pool, the results in the items' order.
#[cfg(feature = "rayon")]
fn runs<I: Send, O: Send>(
    items: Vec<I>,
    threads: usize,
    run: &(impl Fn(Vec<I>) -> Vec<O> + Sync),
) -> Vec<O> {
    use rayon::prelude::*;
    let n = items.len();
    let size = n.div_ceil((threads * 4).max(1)).max(1);
    let mut groups: Vec<Vec<I>> = Vec::with_capacity(n.div_ceil(size));
    let mut items = items.into_iter();
    loop {
        let g: Vec<I> = items.by_ref().take(size).collect();
        if g.is_empty() {
            break;
        }
        groups.push(g);
    }
    groups.into_par_iter().map(run).flatten().collect()
}
