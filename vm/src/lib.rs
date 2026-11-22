//! Rust control plane for vmctl.
//!
//! The crate is the single implementation of the vmctl pipeline: config and
//! fingerprints, the atomic step runner, the device hardware/guest profile,
//! desktop/browser installation, lifecycle control and the CLI helpers.  Tauri
//! commands call this crate; no QEMU argv is assembled outside it.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Message(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub mod accel;
pub mod agentd;
pub mod arch;
pub mod browser;
pub mod chrome_feature;
pub mod chrome_viewer;
pub mod cli;
pub mod compat;
pub mod config;
pub mod console;
pub mod desktop;
pub mod device;
pub mod display;
pub mod download;
pub mod guest_dns;
pub mod ipv4;
pub mod iso;
pub mod journal;
pub mod lock;
pub mod machine;
pub mod machine_details;
pub mod mirror;
pub mod net_forward;
pub mod net_relay;
pub mod netgw;
pub mod network_interfaces;
pub mod network_probe_targets;
pub mod os;
pub mod paths;
pub mod platform;
pub mod preview_forward;
pub mod proxy;
pub mod py_json;
pub mod qemu_engine;
pub mod readiness;
pub mod qmp;
pub mod runner;
pub mod shell;
pub mod shoes;
pub mod sockets;
pub mod startup;
pub mod storage;
pub mod steps;
pub mod util;
pub mod vm;
pub mod workspace;

pub use config::{Config, build_config, load_instance_config, load_instance_meta};
pub use paths::{
    InstancePaths, ResourcePaths, StatePaths, default_home, default_project_root, resolve_home,
    set_project_root,
};
pub use shell::ShellResult;
pub use util::message;
