use serde::{Deserialize, Serialize};

#[cfg(test)]
use ts_rs::TS;

use super::AgentKind;

// ------------------------------------------------------------------ settings

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    /// `host:port` used for TCP-connect latency samples.
    pub latency_target: String,
    /// `host:port` used to check whether the network works directly.
    pub check_target: String,
    pub default_agent: AgentKind,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            latency_target: "1.1.1.1:443".into(),
            check_target: "www.google.com:443".into(),
            default_agent: AgentKind::Claude,
        }
    }
}
