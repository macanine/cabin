//! Storage owned by the Mac application. Guest storage is reported over RPC.

use std::path::{Path, PathBuf};

use serde::Serialize;
use vm_core::machine::Machine;
use vm_core::storage::{self, Inventory, StorageEntry, StorageKind, StorageVolume};

#[derive(Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub(crate) struct StorageReport {
    pub entries: Vec<StorageEntry>,
    #[cfg_attr(test, ts(type = "number"))]
    pub total_bytes: u64,
    pub volume: Option<StorageVolume>,
    pub data_path: String,
    pub app_path: Option<String>,
    pub disk_capacity: Option<String>,
    pub cleanup_blocked: Option<String>,
    #[cfg_attr(test, ts(type = "number"))]
    pub log_retention_days: u64,
}

impl StorageReport {
    pub fn read(machine: &Machine, identifier: &str, cleanup_blocked: Option<String>) -> Self {
        let home = machine.home();
        let app = std::env::current_exe().ok().map(application_path);
        let mut scan = Inventory::default();
        if let Some(path) = &app {
            scan.measure(path, StorageKind::Application, None);
        }
        scan.measure_vm(home);
        if let Some(caches) = dirs::cache_dir() {
            // WebView2 on Windows, WKWebView on macOS.
            let (webkit_cache, webkit_data) = if cfg!(windows) {
                let webview = caches.join(identifier).join("EBWebView");
                (webview.join("Default").join("Cache"), webview)
            } else {
                let library = caches.parent().unwrap_or(&caches);
                (
                    caches.join(identifier).join("WebKit"),
                    library.join("WebKit").join(identifier).join("WebsiteData"),
                )
            };
            scan.measure(&webkit_cache, StorageKind::WebkitCache, None);
            scan.measure(&webkit_data, StorageKind::WebkitData, None);
            scan.measure(&caches.join("cabin"), StorageKind::DeveloperCache, app.as_deref());
        }
        let entries = scan.finish(home);
        let total_bytes = entries
            .iter()
            .filter(|entry| entry.kind != StorageKind::DeveloperCache)
            .fold(0_u64, |total, entry| total.saturating_add(entry.bytes));
        Self {
            entries,
            total_bytes,
            volume: storage::volume(home),
            data_path: home.display().to_string(),
            app_path: app.map(|path| path.display().to_string()),
            disk_capacity: machine.config().ok().map(|cfg| cfg.disk_size),
            cleanup_blocked,
            log_retention_days: storage::LOG_RETENTION_DAYS,
        }
    }
}

fn application_path(executable: PathBuf) -> PathBuf {
    executable
        .ancestors()
        .find(|candidate| {
            candidate.extension().is_some_and(|ext| ext == "app")
                && executable
                    .strip_prefix(candidate)
                    .is_ok_and(|rest| rest.starts_with(Path::new("Contents/MacOS")))
        })
        .map(Path::to_path_buf)
        .unwrap_or(executable)
}
