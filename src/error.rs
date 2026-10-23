//! Error type shared by the library and the CLI JSON envelope.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Failure from argument checking or desktop automation.
#[derive(Debug, Error)]
pub enum ComputerError {
    /// The action or flag value cannot be used.
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),

    /// The operating system or a backend call failed.
    #[error("execution failed: {0}")]
    ExecutionFailed(String),

    /// A filesystem or process call failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// The platform does not implement this operation.
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// No running app matched the name, bundle id, or pid.
    #[error("app not found: {0}")]
    AppNotFound(String),

    /// The app resolved, but the window did not.
    #[error("window not found: {0}")]
    WindowNotFound(String),

    /// The element index is missing from the fresh tree.
    #[error("element not found: {0}")]
    ElementNotFound(String),

    /// The element index exists, but its role, identifier, or frame changed.
    #[error("stale element: {0}")]
    StaleElement(String),

    /// Accessibility or Screen Recording permission is off.
    #[error("permission required: {0}")]
    PermissionRequired(String),

    /// A built-in or configured policy refused the action.
    #[error("policy denied: {0}")]
    PolicyDenied(String),

    /// A sensitive action needs a confirm hook and none is configured.
    #[error("confirm required: {0}")]
    ConfirmRequired(String),

    /// The confirm hook exited non-zero or timed out.
    #[error("confirm denied: {0}")]
    ConfirmDenied(String),

    /// A browser URL matched the block list.
    #[error("blocked url: {0}")]
    BlockedUrl(String),

    /// Local user input tripped the kill switch.
    #[error("paused: {0}")]
    Paused(String),

    /// Chrome DevTools is not enabled or no debugging port is open.
    #[error("cdp unavailable: {0}")]
    CdpUnavailable(String),

    /// The screen is locked, asleep, or no display is attached.
    #[error("display unavailable: {0}")]
    DisplayUnavailable(String),

    /// A browser wait or action exceeded its timeout.
    #[error("timeout: {0}")]
    Timeout(String),

    /// `verify` did not observe its condition before the timeout.
    #[error("verification failed: {0}")]
    VerificationFailed(String),
}

impl ComputerError {
    /// Stable machine-readable code used in JSON error objects.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidArguments(_) => "invalid_arguments",
            Self::ExecutionFailed(_) => "execution_failed",
            Self::Io(_) => "io_error",
            Self::Unsupported(_) => "unsupported",
            Self::AppNotFound(_) => "app_not_found",
            Self::WindowNotFound(_) => "window_not_found",
            Self::ElementNotFound(_) => "element_not_found",
            Self::StaleElement(_) => "stale_element",
            Self::PermissionRequired(_) => "permission_required",
            Self::PolicyDenied(_) => "policy_denied",
            Self::ConfirmRequired(_) => "confirm_required",
            Self::ConfirmDenied(_) => "confirm_denied",
            Self::BlockedUrl(_) => "blocked_url",
            Self::Paused(_) => "paused",
            Self::CdpUnavailable(_) => "cdp_unavailable",
            Self::DisplayUnavailable(_) => "display_unavailable",
            Self::Timeout(_) => "timeout",
            Self::VerificationFailed(_) => "verification_failed",
        }
    }
}

/// `error` object inside a failed [`crate::Response`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// Stable code such as `invalid_arguments` or `execution_failed`.
    pub code: String,
    /// Human-readable explanation.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_display_match_the_variant() {
        let err = ComputerError::InvalidArguments("missing text".into());
        assert_eq!(err.code(), "invalid_arguments");
        assert_eq!(err.to_string(), "invalid arguments: missing text");

        let err = ComputerError::ExecutionFailed("click failed".into());
        assert_eq!(err.code(), "execution_failed");
        assert_eq!(err.to_string(), "execution failed: click failed");

        let err = ComputerError::Unsupported("no at-spi".into());
        assert_eq!(err.code(), "unsupported");
        assert_eq!(err.to_string(), "unsupported: no at-spi");

        let err = ComputerError::Io(std::io::Error::other("disk"));
        assert_eq!(err.code(), "io_error");
        assert!(err.to_string().contains("disk"));

        assert_eq!(
            ComputerError::AppNotFound("Notes".into()).code(),
            "app_not_found"
        );
        assert_eq!(
            ComputerError::WindowNotFound("0".into()).code(),
            "window_not_found"
        );
        assert_eq!(
            ComputerError::ElementNotFound("4".into()).code(),
            "element_not_found"
        );
        assert_eq!(
            ComputerError::StaleElement("role changed".into()).code(),
            "stale_element"
        );
        assert_eq!(
            ComputerError::PermissionRequired("Accessibility".into()).code(),
            "permission_required"
        );
        assert_eq!(
            ComputerError::PolicyDenied("Terminal".into()).code(),
            "policy_denied"
        );
        assert_eq!(
            ComputerError::ConfirmRequired("quit".into()).code(),
            "confirm_required"
        );
        assert_eq!(
            ComputerError::ConfirmDenied("timeout".into()).code(),
            "confirm_denied"
        );
        assert_eq!(
            ComputerError::BlockedUrl("https://accounts.example.com".into()).code(),
            "blocked_url"
        );
        assert_eq!(ComputerError::Paused("user input".into()).code(), "paused");
        assert_eq!(
            ComputerError::CdpUnavailable("feature off".into()).code(),
            "cdp_unavailable"
        );
        let err = ComputerError::DisplayUnavailable("the screen is locked".into());
        assert_eq!(err.code(), "display_unavailable");
        assert_eq!(err.to_string(), "display unavailable: the screen is locked");
    }
}
