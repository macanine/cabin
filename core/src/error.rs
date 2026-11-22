//! Typed errors with stable codes for the IPC boundary.

use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("{0} not found")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("operation cancelled")]
    Cancelled,
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("{message}")]
    Command { message: String, output: String },
    #[error("{0}")]
    Network(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Stable machine-readable code, shared with the TypeScript boundary.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid",
            Self::NotFound(_) => "notFound",
            Self::Conflict(_) => "conflict",
            Self::Cancelled => "cancelled",
            Self::Io { .. } => "io",
            Self::Db(_) => "storage",
            Self::Command { .. } => "command",
            Self::Network(_) => "network",
            Self::Unavailable(_) => "unavailable",
            Self::Internal(_) => "internal",
        }
    }

    /// Extra detail that is useful in a log view but too long for a toast.
    pub fn details(&self) -> Option<String> {
        match self {
            Self::Command { output, .. } if !output.trim().is_empty() => {
                Some(output.trim().to_string())
            }
            Self::Io { source, .. } => Some(source.to_string()),
            _ => None,
        }
    }

    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    pub fn path(action: &str, path: &Path, source: std::io::Error) -> Self {
        Self::io(format!("{action} {}", path.display()), source)
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Invalid(format!("invalid JSON: {error}"))
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::io("I/O error", error)
    }
}
