use serde::{Deserialize, Serialize};

/// Structured error on the IPC boundary: a stable `code`, a human message,
/// and optional longer `details` (command output, OS error).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub(crate) struct AppError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub details: Option<String>,
}

impl AppError {
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: "internal".into(),
            message: message.into(),
            details: None,
        }
    }
}

impl From<agent_core::Error> for AppError {
    fn from(error: agent_core::Error) -> Self {
        Self {
            code: error.code().into(),
            message: error.to_string(),
            details: error.details(),
        }
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
