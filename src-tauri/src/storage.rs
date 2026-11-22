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
        if let Some(user_home) = vm_core::compat::home_dir().map(PathBuf::from) {
            #[cfg(windows)]
            {
                // WebView2 keeps the web data under %LOCALAPPDATA%/<identifier>.
                let local = std::env::var_os("LOCALAPPDATA")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| user_home.join("AppData").join("Local"));
                let webview = local.join(identifier).join("EBWebView");
                scan.measure(&webview.join("Default").join("Cache"), StorageKind::WebkitCache, None);
                scan.measure(&webview, StorageKind::WebkitData, None);
                scan.measure(&local.join("cabin"), StorageKind::DeveloperCache, app.as_deref());
            }
            #[cfg(not(windows))]
            {
                let library = user_home.join("Library");
                let caches = library.join("Caches");
                scan.measure(
                    &caches.join(identifier).join("WebKit"),
                    StorageKind::WebkitCache,
                    None,
                );
                scan.measure(
                    &library.join("WebKit").join(identifier).join("WebsiteData"),
                    StorageKind::WebkitData,
                    None,
                );
                scan.measure(
                    &caches.join("cabin"),
                    StorageKind::DeveloperCache,
                    app.as_deref(),
                );
            }
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
