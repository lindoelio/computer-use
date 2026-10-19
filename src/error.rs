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
}

impl ComputerError {
    /// Stable machine-readable code used in JSON error objects.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidArguments(_) => "invalid_arguments",
            Self::ExecutionFailed(_) => "execution_failed",
            Self::Io(_) => "io_error",
            Self::Unsupported(_) => "unsupported",
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
    }
}
