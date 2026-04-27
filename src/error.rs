//! Error and Result types for mdkit.
//!
//! All public APIs that can fail return [`Result<T>`]. Errors are
//! categorized broadly so callers can map them to user-facing messages
//! without pattern-matching on opaque strings.

use std::io;
use thiserror::Error;

/// Result alias used across the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can arise during extraction.
///
/// The variants are intentionally coarse-grained — a `ParseError` from
/// the Pandoc backend and a `ParseError` from `pdfium-render` look the
/// same to the caller, distinguished only by the message string. If
/// you need fine-grained failure routing, dispatch on
/// [`Extractor::name`](crate::Extractor::name) when constructing the
/// engine, or wrap a backend with your own error mapping.
///
/// Marked `#[non_exhaustive]` so future minor versions can add new
/// variants (e.g. a dedicated `EncryptedDocument` for password-
/// protected PDFs) without breaking downstream `match` blocks.
/// Callers that pattern-match should always include a wildcard arm.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// Filesystem failure: file missing, permission denied, EOF, etc.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// The file extension has no registered extractor on this engine.
    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),

    /// A specific extractor doesn't support the operation (typically
    /// `extract_bytes` on a backend that only handles file paths).
    #[error("unsupported operation: {0}")]
    UnsupportedOperation(String),

    /// Backend-specific parse failure. The string is the backend's
    /// error message verbatim.
    #[error("parse error: {0}")]
    ParseError(String),

    /// A required external dependency (Pandoc binary, Tesseract, ONNX
    /// model file) is missing or unusable. `name` is the dependency
    /// label; `details` is the underlying error.
    #[error("missing dependency `{name}`: {details}")]
    MissingDependency {
        /// Human-readable dependency name (`"pandoc"`, `"libpdfium.dylib"`).
        name: String,
        /// Underlying error or the path that was checked.
        details: String,
    },

    /// A sidecar process exited with a non-zero status. `code` is the
    /// exit code if known.
    #[error("sidecar `{name}` failed with exit code {code:?}: {stderr}")]
    SidecarFailure {
        /// Backend name (e.g. `"pandoc"`).
        name: String,
        /// Exit code if the process exited normally; `None` if killed
        /// by signal.
        code: Option<i32>,
        /// Captured stderr, truncated to a sensible length by the
        /// caller before being attached.
        stderr: String,
    },

    /// Catch-all for backends that need to surface something the other
    /// variants don't capture.
    #[error("{0}")]
    Other(String),
}

impl Error {
    /// Convenience constructor for `Error::ParseError`.
    pub fn parse(msg: impl Into<String>) -> Self {
        Self::ParseError(msg.into())
    }

    /// Convenience constructor for `Error::Other`.
    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_errors_convert_via_from() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "missing");
        let err: Error = io_err.into();
        assert!(matches!(err, Error::Io(_)));
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn unsupported_format_renders_path() {
        let err = Error::UnsupportedFormat(".xyz".into());
        assert!(err.to_string().contains(".xyz"));
    }

    #[test]
    fn missing_dependency_renders_name_and_details() {
        let err = Error::MissingDependency {
            name: "pandoc".into(),
            details: "binary not found in PATH".into(),
        };
        let s = err.to_string();
        assert!(s.contains("pandoc"));
        assert!(s.contains("PATH"));
    }
}
