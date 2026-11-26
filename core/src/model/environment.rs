use serde::{Deserialize, Serialize};

#[cfg(test)]
use ts_rs::TS;

// --------------------------------------------------------------- environment

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentSpec {
    pub timezone: String,
    pub language: String,
}

impl Default for EnvironmentSpec {
    fn default() -> Self {
        Self {
            timezone: "UTC".into(),
            language: "en_US.UTF-8".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentState {
    /// The last successfully applied environment.
    pub committed: EnvironmentSpec,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub applied_at: Option<i64>,
    /// What the operating system reports right now.
    pub system_timezone: Option<String>,
    pub system_language: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct FieldState {
    pub field: String,
    pub enabled: bool,
    pub reason: Option<String>,
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentReview {
    pub fields: Vec<FieldState>,
    pub changes: Vec<String>,
    pub blocked: Option<String>,
    pub timezones: Vec<String>,
    pub languages: Vec<String>,
}
