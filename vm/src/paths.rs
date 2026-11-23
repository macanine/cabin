//! Filesystem layout for bundled resources and mutable application data.
//!
//! Packaged builds keep resources read-only while instances, proxy state and
//! logs live in application data. Keeping both roots in one type prevents
//! callers from guessing which location owns a file.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static PROJECT_ROOT_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();
static HOME_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

fn nonempty_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub fn set_project_root(path: PathBuf) {
    let _ = PROJECT_ROOT_OVERRIDE.set(path);
}

pub fn default_project_root() -> PathBuf {
    if let Some(root) = PROJECT_ROOT_OVERRIDE.get() {
        return root.clone();
    }
    if let Some(root) = nonempty_env("VMCTL_PROJECT_ROOT") {
        return root;
    }
    // In the source tree the crate directory holds `config/` and `assets/`;
    // packaged builds call `set_project_root` with the bundle's resources.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub fn default_home() -> PathBuf {
    if let Some(home) = HOME_OVERRIDE.get() {
        return home.clone();
    }
    if let Some(home) = nonempty_env("VMCTL_HOME") {
        return home;
    }
    default_project_root().join(".vmctl")
}

pub fn resolve_home(value: Option<&str>) -> PathBuf {
    value
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default_home)
}

/// Read-only files shipped with the source tree or Tauri application bundle.
#[derive(Debug, Clone)]
pub struct ResourcePaths {
    root: PathBuf,
}

impl ResourcePaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn current() -> Self {
        Self::new(default_project_root())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn default_config(&self) -> PathBuf {
        self.root.join("config/default.json")
    }

    pub fn browser_config(&self) -> PathBuf {
        self.root.join("config/browser.json")
    }

    pub fn device_config(&self) -> PathBuf {
        self.root.join("config/device.json")
    }

    pub fn image(&self, file_name: &str) -> PathBuf {
        self.root.join("tools/images").join(file_name)
    }

    pub fn shoes_binary(&self) -> PathBuf {
        self.root.join("tools/shoes/bin/shoes")
    }
}

/// Mutable per-user state, caches and logs.
#[derive(Debug, Clone)]
pub struct StatePaths {
    root: PathBuf,
}

/// Files owned by one VM instance.
#[derive(Debug, Clone)]
pub struct InstancePaths {
    root: PathBuf,
}

impl InstancePaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn file(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    pub fn steps(&self) -> PathBuf {
        self.root.join("steps")
    }

    pub fn meta(&self) -> PathBuf {
        self.root.join("instance.json")
    }

    pub fn disk(&self) -> PathBuf {
        self.root.join("disk.qcow2")
    }

    pub fn runtime(&self) -> PathBuf {
        self.root.join("runtime.json")
    }

    pub fn efi_vars(&self) -> PathBuf {
        self.root.join("efi-vars.fd")
    }

    pub fn seed(&self) -> PathBuf {
        self.root.join("seed.iso")
    }

    pub fn user_data(&self) -> PathBuf {
        self.root.join("user-data")
    }

    pub fn meta_data(&self) -> PathBuf {
        self.root.join("meta-data")
    }

    pub fn network_config(&self) -> PathBuf {
        self.root.join("network-config")
    }

    pub fn ssh_key(&self) -> PathBuf {
        self.root.join("id_ed25519")
    }

    pub fn ssh_public_key(&self) -> PathBuf {
        self.root.join("id_ed25519.pub")
    }

    pub fn credentials(&self) -> PathBuf {
        self.root.join("credentials.json")
    }

    pub fn provisioned(&self) -> PathBuf {
        self.root.join("provisioned.json")
    }

    pub fn pidfile(&self) -> PathBuf {
        self.root.join("qemu.pid")
    }

    pub fn qmp_socket(&self) -> PathBuf {
        self.root.join("qmp.sock")
    }

    pub fn qga_socket(&self) -> PathBuf {
        self.root.join("qga.sock")
    }

    pub fn console_socket(&self) -> PathBuf {
        self.root.join("console.sock")
    }

    pub fn spice_dir(&self) -> PathBuf {
        self.root.join("spice")
    }

    pub fn provision_serial(&self) -> PathBuf {
        self.root.join("serial-provision.log")
    }

    pub fn identity(&self) -> PathBuf {
        self.root.join("identity.json")
    }

    pub fn desktop(&self) -> PathBuf {
        self.root.join("desktop.json")
    }

    pub fn browser(&self) -> PathBuf {
        self.root.join("browser.json")
    }

    pub fn browser_lock(&self) -> PathBuf {
        self.root.join("browser.lock")
    }

    pub fn device(&self) -> PathBuf {
        self.root.join("device")
    }

    pub fn legacy_device(&self) -> PathBuf {
        self.root.join("\x73\x74\x65\x61\x6c\x74\x68")
    }

    pub fn mappings(&self) -> PathBuf {
        self.root.join("mappings.json")
    }

    pub fn workspace(&self) -> PathBuf {
        self.root.join("workspace.json")
    }
}

impl StatePaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn instances(&self) -> PathBuf {
        self.root.join("instances")
    }

    pub fn instance(&self, name: &str) -> InstancePaths {
        InstancePaths::new(self.instances().join(name))
    }

    pub fn instance_steps(&self, name: &str) -> PathBuf {
        self.instance(name).steps()
    }

    pub fn instance_meta(&self, name: &str) -> PathBuf {
        self.instance(name).meta()
    }

    pub fn images(&self) -> PathBuf {
        self.root.join("images")
    }

    /// Built-in tools the app downloads (the QEMU engine).
    pub fn engines(&self) -> PathBuf {
        self.root.join("engines")
    }

    pub fn image(&self, file_name: &str) -> PathBuf {
        self.images().join(file_name)
    }

    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn instance_logs(&self, name: &str) -> PathBuf {
        self.logs().join(name)
    }

    pub fn tmp(&self) -> PathBuf {
        self.root.join("tmp")
    }

    pub fn bin(&self) -> PathBuf {
        self.root.join("bin")
    }

    pub fn journal(&self) -> PathBuf {
        self.logs().join("operations.jsonl")
    }

    pub fn legacy_journal(&self) -> PathBuf {
        self.root.join("journal.jsonl")
    }

    pub fn operation_lock(&self) -> PathBuf {
        self.root.join("lock")
    }

    pub fn global_state(&self) -> PathBuf {
        self.root.join("state/global")
    }

    pub fn proxy(&self) -> PathBuf {
        self.root.join("proxy")
    }

    pub fn proxy_runtime(&self) -> PathBuf {
        self.proxy().join("runtime.json")
    }

    pub fn proxy_log(&self) -> PathBuf {
        self.instance_logs("global").join("shoes.log")
    }

    pub fn proxy_transaction_lock(&self) -> PathBuf {
        self.proxy().join("transaction.lock")
    }

    pub fn shoes(&self) -> PathBuf {
        self.proxy().join("shoes")
    }

    pub fn shoes_sources(&self) -> PathBuf {
        self.shoes().join("sources.json")
    }

    pub fn shoes_selection(&self) -> PathBuf {
        self.shoes().join("selection.json")
    }

    pub fn shoes_config(&self) -> PathBuf {
        self.shoes().join("managed.yaml")
    }
}

#[cfg(test)]
mod tests {
    use super::{ResourcePaths, StatePaths};
    use std::path::Path;

    #[test]
    fn packaged_resources_and_mutable_state_have_distinct_owners() {
        let resources = ResourcePaths::new("/Applications/vmctl.app/Resources");
        let state = StatePaths::new("/Users/test/Library/Application Support/vmctl");

        assert_eq!(
            resources.shoes_binary(),
            Path::new("/Applications/vmctl.app/Resources/tools/shoes/bin/shoes")
        );
        assert_eq!(
            state.shoes_sources(),
            Path::new("/Users/test/Library/Application Support/vmctl/proxy/shoes/sources.json")
        );
        assert_eq!(
            state.instance("demo").root(),
            Path::new("/Users/test/Library/Application Support/vmctl/instances/demo")
        );
    }
}
