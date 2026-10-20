//! Process-wide current-thread tokio runtime for the Linux backend.
//!
//! AT-SPI, zbus, and the `atspi` crate are async, but the rest of the
//! project stays sync: the dispatcher calls into the Linux backend from the
//! CLI thread, and `impl_platform_backend` returns `Result<_, ComputerError>`.
//! Bridging the two means running the async work on a small dedicated tokio
//! runtime that is created lazily on first use and reused for the life of
//! the process.
//!
//! The runtime is current-thread so we never spawn executor threads in the
//! background. `block_on` blocks the CLI thread for the duration of one
//! call, which is fine — desktop operations are user-paced and never
//! observably long-lived. A real long-running listener (event
//! subscriptions) would live here too, but P13a only needs it for short
//! request/response AT-SPI calls.
//!
//! `block_on` returns [`ComputerError::ExecutionFailed`] when the runtime
//! itself could not be created, preserving the rest of the uniform error
//! surface that other Linux errors already use. The runtime is rebuilt on
//! the next call after such a failure because the [`OnceLock`] only caches
//! successful builds.
//!
//! See [`crate::backend::linux::atspi`] for the only current call site.

use std::sync::OnceLock;

use thiserror::Error;

use crate::error::ComputerError;

/// Storage for the lazily-built runtime or its build error.
type RuntimeCell = Result<tokio::runtime::Runtime, RuntimeBuildError>;

/// Static handle to the lazily-built current-thread tokio runtime.
static RUNTIME: OnceLock<RuntimeCell> = OnceLock::new();

/// Why the runtime could not be built.
#[derive(Debug, Error, Clone)]
pub enum RuntimeBuildError {
    /// `tokio::runtime::Builder::build()` failed. The cause is reported.
    #[error("failed to build current-thread tokio runtime: {0}")]
    Build(#[source] std::io::Error),
}

/// Run one async closure to completion on the lazily-built runtime.
///
/// Errors from the futures passed in propagate to the caller as-is. The
/// runtime is built on the first call only — subsequent calls reuse the
/// same single-thread executor.
pub fn block_on<F>(future: F) -> Result<F::Output, ComputerError>
where
    F: std::future::Future + 'static,
    F::Output: Send + 'static,
{
    let runtime = get_or_build_runtime()?;
    match runtime.block_on(future) {
        Ok(value) => Ok(value),
        // `block_on` itself does not return `Err`; this branch is defensive
        // against future runtime changes and would only fire when someone
        // manages to make the inner future panic.
        Err(_panic) => Err(ComputerError::ExecutionFailed(
            "linux tokio runtime rejected the future".into(),
        )),
    }
}

/// Borrow the process runtime handle. Builds it on first call.
fn get_or_build_runtime() -> Result<&'static tokio::runtime::Runtime, ComputerError> {
    let cell = RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(RuntimeBuildError::Build)
    });
    cell.as_ref()
        .map_err(|build| ComputerError::ExecutionFailed(build.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The runtime must build and execute a trivial future on this Mac and on
    /// Linux CI even when no display is present.
    #[test]
    fn block_on_executes_simple_future() {
        let value = block_on(async { 7_u32 + 11 }).unwrap();
        assert_eq!(value, 18);
    }

    /// Blocking on a future that returns a `Result<(), ()>` must surface the
    /// inner `Err` — the runtime does not swallow it.
    #[test]
    fn block_on_propagates_inner_err() {
        let outcome: Result<i32, &'static str> = block_on(async { Err("boom") }).unwrap();
        assert_eq!(outcome, Err("boom"));
    }

    /// The runtime must be built exactly once across calls.
    #[test]
    fn handle_is_shared() {
        let _ = block_on(async { 1_u32 }).unwrap();
        let _ = block_on(async { 2_u32 }).unwrap();
        // The cached handle must be the same address; rebuilds would leak
        // worker threads.
        let first = RUNTIME.get().expect("runtime must be initialised");
        let second = RUNTIME.get().expect("runtime must be initialised");
        assert!(std::ptr::eq(
            first.as_ref().unwrap(),
            second.as_ref().unwrap()
        ));
    }
}
