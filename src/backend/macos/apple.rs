//! `osascript` wrapper for `apple-event`.
//!
//! The script body is passed to `osascript` with `-e` as a single argv entry
//! (and `-l JavaScript` when the caller asked for JXA). The binary is never
//! run through a shell. The script body is never logged verbatim; the audit
//! line records its byte length and a stable hash instead.

use std::time::Duration;

use serde::Serialize;

use crate::backend::command::stderr_snippet;
use crate::error::ComputerError;
use crate::protocol::{AppleEventLanguage, APPLE_EVENT_TIMEOUT_SECS};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct AppleEventOutcome {
    pub(crate) script_bytes: usize,
    /// Hex FNV-1a 64-bit hash of the script bytes. Stable across runs without
    /// pulling in a cryptographic hash dependency. Identifies a script in
    /// audit lines without leaking its body.
    pub(crate) script_hash: String,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) exit_status: i32,
}

/// Run `osascript -e <script>` (or `-l JavaScript -e <script>`) and collect
/// its output. The result carries a script hash so the audit line can identify
/// a script without leaking its body.
pub(crate) fn run(
    script: &str,
    language: AppleEventLanguage,
) -> Result<AppleEventOutcome, ComputerError> {
    if script.trim().is_empty() {
        return Err(ComputerError::InvalidArguments(
            "apple-event script is empty".into(),
        ));
    }
    let timeout = Duration::from_secs(APPLE_EVENT_TIMEOUT_SECS);
    let mut argv: Vec<&str> = Vec::with_capacity(4);
    argv.push("osascript");
    if matches!(language, AppleEventLanguage::JavaScript) {
        argv.push("-l");
        argv.push("JavaScript");
    }
    argv.push("-e");
    argv.push(script);
    let output = crate::backend::command::run_timeout("osascript", &argv, timeout)
        .map_err(ComputerError::ExecutionFailed)?;
    let exit_status = output.status.code().unwrap_or(-1);
    let stdout = trim_one_line(&output.stdout);
    let stderr = stderr_snippet(&output.stderr);
    if !output.status.success() {
        return Err(ComputerError::ExecutionFailed(format!(
            "osascript failed (exit {exit_status}): {stderr}"
        )));
    }
    let script_bytes = script.len();
    let script_hash = fnv1a64_hex(script.as_bytes());
    Ok(AppleEventOutcome {
        script_bytes,
        script_hash,
        stdout,
        stderr,
        exit_status,
    })
}

fn trim_one_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.chars().count() > 4096 {
        let truncated: String = trimmed.chars().take(4096).collect();
        format!("{truncated}...")
    } else {
        trimmed.to_string()
    }
}

/// FNV-1a 64-bit hash, lowercase hex. Stable and deterministic across runs
/// and platforms without a cryptographic hash dependency. The hash is a
/// script identifier, not a security primitive.
fn fnv1a64_hex(bytes: &[u8]) -> String {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_script_is_rejected() {
        let error = run("", AppleEventLanguage::AppleScript).unwrap_err();
        assert!(matches!(error, ComputerError::InvalidArguments(_)));
    }

    #[test]
    fn whitespace_script_is_rejected() {
        let error = run("   \n  ", AppleEventLanguage::AppleScript).unwrap_err();
        assert!(matches!(error, ComputerError::InvalidArguments(_)));
    }

    #[test]
    fn fnv1a64_hex_is_deterministic_and_collides_loudly() {
        let a = fnv1a64_hex(b"hello");
        let b = fnv1a64_hex(b"hello");
        assert_eq!(a, b);
        let c = fnv1a64_hex(b"world");
        assert_ne!(a, c);
        assert_eq!(a.len(), 16);
    }
}
