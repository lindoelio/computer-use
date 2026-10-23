//! Run a Shortcuts workflow by name through the `shortcuts` binary.
//!
//! `shortcuts run` is the documented entry point. The `shortcuts` binary is
//! invoked with the workflow name as a single positional argument. A missing
//! binary surfaces as `execution_failed` rather than an `unsupported` error:
//! the binary is part of macOS but a process running in a sandboxed context
//! may still see it as absent.

use std::time::Duration;

use serde::Serialize;

use crate::backend::command::stderr_snippet;
use crate::error::ComputerError;
use crate::protocol::SHORTCUTS_TIMEOUT_SECS;

/// What `shortcuts run` returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShortcutsReport {
    /// Exit status of `shortcuts run`.
    pub exit_status: i32,
    /// Trimmed stderr, never the script body.
    pub stderr: String,
}

/// Run `shortcuts run <name>` and wait for completion. The process is killed
/// after [`SHORTCUTS_TIMEOUT_SECS`] (60 seconds).
pub fn run(name: &str) -> Result<ShortcutsReport, ComputerError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(ComputerError::InvalidArguments(
            "shortcuts run requires a workflow name".into(),
        ));
    }
    let timeout = Duration::from_secs(SHORTCUTS_TIMEOUT_SECS);
    let output = crate::backend::command::run_timeout("shortcuts", &["run", trimmed], timeout)
        .map_err(ComputerError::ExecutionFailed)?;
    let exit_status = output.status.code().unwrap_or(-1);
    let stderr = stderr_snippet(&output.stderr);
    if !output.status.success() {
        return Err(ComputerError::ExecutionFailed(format!(
            "shortcuts run {trimmed} failed (exit {exit_status}): {stderr}"
        )));
    }
    Ok(ShortcutsReport {
        exit_status,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_name_is_an_argument_error() {
        let error = run("").unwrap_err();
        assert!(matches!(error, ComputerError::InvalidArguments(_)));
    }

    #[test]
    fn whitespace_name_is_an_argument_error() {
        let error = run("   ").unwrap_err();
        assert!(matches!(error, ComputerError::InvalidArguments(_)));
    }
}
