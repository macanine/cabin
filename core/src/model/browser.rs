use serde::{Deserialize, Serialize};

#[cfg(test)]
use ts_rs::TS;

/// The browser-visible values compared with the installed browser settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct BrowserEnvironmentState {
    pub timezone: Option<String>,
    pub language: Option<String>,
    pub languages: Vec<String>,
    pub platform: Option<String>,
}

/// The managed browser's editable settings (the Mac's configuration). With
/// `followMachine` the region, hardware hints and platform come from the
/// machine and keep following it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct BrowserSettings {
    pub follow_machine: bool,
    pub timezone: String,
    pub locale: String,
    pub user_agent: String,
    pub platform: String,
    pub hardware_concurrency: u32,
    pub device_memory: f64,
}

/// Everything the browser environment panel shows: the settings, and (when
/// asked to check and the machine runs) the installed and live state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct BrowserEnvironmentView {
    pub enabled: bool,
    pub machine_running: bool,
    pub settings: Option<BrowserSettings>,
    pub probe: Option<BrowserEnvironmentProbe>,
    /// Whether the live pages use the installed settings; `None` when no
    /// live page was observed. Judged on the Mac, like the network check.
    pub live_matches: Option<bool>,
    /// Unix milliseconds of `probe`.
    #[cfg_attr(test, ts(type = "number | null"))]
    pub checked_at: Option<i64>,
}

/// What the live Chrome could tell. Only `Observed` carries values; the
/// other states are normal (Chrome closed, no page, tabs asleep) and never
/// block the machine's internet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum BrowserLive {
    Observed,
    #[default]
    NotRunning,
    /// Running, but no web page answered (none open, or asleep).
    Idle,
    /// Could not be judged (pages disagree, the audit failed).
    Unavailable,
}

/// The managed browser's environment, as the network check sees it.
///
/// `installed` (the guest's profile file) against `expected` (the Mac's
/// configuration) decides whether the browser is set up correctly, and that
/// is what the gate requires. `observed` is the live Chrome: advisory only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct BrowserEnvironmentProbe {
    pub enabled: bool,
    pub expected: BrowserEnvironmentState,
    /// The installed profile in the guest; `None` when it could not be read.
    pub installed: Option<BrowserEnvironmentState>,
    pub live: BrowserLive,
    pub observed: Option<BrowserEnvironmentState>,
    /// Why the live browser has no observation, in a few words.
    pub live_detail: Option<String>,
    /// A problem with the installed settings (blocks: they must be right).
    pub error: Option<String>,
}
