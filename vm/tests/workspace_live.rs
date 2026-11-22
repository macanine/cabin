//! Explicit live test for P0/P1/P2 mappings and bidirectional host writes.
//!
//! Run with:
//!   VMCTL_E2E_HOME=/tmp/example VMCTL_E2E_INSTANCE=workspace \
//!     cargo test --test workspace_live -- --ignored --nocapture

use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use vm_core::config::{default_project_root, load_instance_config};
use vm_core::vm::{Boot, Runtime};
use vm_core::workspace;

#[test]
#[ignore = "requires an explicit stopped test VM; starts it with the standard mappings"]
fn p0_p1_p2_mappings_are_bidirectional_and_custom_paths_are_preserved() {
    let home = PathBuf::from(
        std::env::var("VMCTL_E2E_HOME").expect("VMCTL_E2E_HOME must point to the vmctl home"),
    );
    let name =
        std::env::var("VMCTL_E2E_INSTANCE").expect("VMCTL_E2E_INSTANCE must name the test VM");
    let cfg =
        load_instance_config(&default_project_root(), &home, &name).expect("load instance config");
    let runtime = Runtime::new(&cfg);

    let log_path = cfg.instance_dir().join("workspace-live-test.log");
    let log = Arc::new(Mutex::new(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .expect("open test log"),
    ));
    if runtime.running() {
        runtime.stop(&log, Duration::from_secs(60));
    }
    for mapping in workspace::mappings(&cfg).expect("read existing mappings") {
        workspace::remove_mapping(&home, &name, &mapping.tag).expect("remove test mapping");
    }
    assert!(
        workspace::mappings(&cfg).expect("read mappings").is_empty(),
        "the explicit live-test instance must start without mappings"
    );

    let root = default_project_root();
    let hosts = [root.join("core"), root.join("src"), root.join("src-tauri")];
    for host in &hosts {
        workspace::save_mapping(&home, &name, &host.display().to_string(), None)
            .expect("save default P0/P1/P2 mapping");
    }
    let mapped = workspace::mappings(&cfg).expect("read P0/P1/P2 mappings");
    assert_eq!(
        mapped
            .iter()
            .map(|mapping| mapping.guest_path.as_str())
            .collect::<Vec<_>>(),
        vec!["/root/P0", "/root/P1", "/root/P2"]
    );

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let custom_host = std::env::temp_dir().join(format!("vmctl-custom-map-{unique}"));
    fs::create_dir(&custom_host).expect("create custom mapping folder");
    workspace::save_mapping(
        &home,
        &name,
        &custom_host.display().to_string(),
        Some("/root/projects/custom"),
    )
    .expect("save custom mapping");
    let canonical_custom_host = custom_host
        .canonicalize()
        .expect("canonicalize custom host")
        .display()
        .to_string();
    let custom = workspace::mappings(&cfg)
        .expect("read custom mapping")
        .into_iter()
        .find(|mapping| mapping.host_path == canonical_custom_host)
        .expect("custom mapping");
    assert_eq!(custom.guest_path, "/root/projects/custom");
    workspace::remove_mapping(&home, &name, &custom.tag).expect("remove custom mapping");
    fs::remove_dir(&custom_host).expect("remove custom host folder");

    assert!(
        runtime
            .start(&log, Boot::new(Duration::from_secs(120)))
            .expect("start test VM")
    );
    workspace::mount_mappings(&cfg, &runtime).expect("mount P0/P1/P2 mappings");

    let host_name = format!("vmctl-host-to-guest-{unique}.txt");
    let guest_name = format!("vmctl-guest-to-host-{unique}.txt");
    for host in &hosts {
        fs::write(host.join(&host_name), format!("host-write:{unique}"))
            .expect("write host marker");
    }
    let verify_host_writes = format!(
        "sudo -n sh -c 'set -eu; for p in /root/P0 /root/P1 /root/P2; do test \"$(cat \"$p/{host_name}\")\" = \"host-write:{unique}\"; done'"
    );
    let (code, output) = runtime.ssh_capture(&verify_host_writes, Duration::from_secs(20));
    assert_eq!(code, 0, "guest could not read host writes: {output}");

    let write_from_guest = format!(
        "sudo -n sh -c 'set -eu; for p in /root/P0 /root/P1 /root/P2; do printf \"guest-write:%s\\n\" \"{}\" > \"$p/{guest_name}\"; done'",
        unique
    );
    let (code, output) = runtime.ssh_capture(&write_from_guest, Duration::from_secs(20));
    assert_eq!(code, 0, "guest could not write mapped folders: {output}");
    for host in &hosts {
        assert_eq!(
            fs::read_to_string(host.join(&guest_name)).expect("read guest marker"),
            format!("guest-write:{unique}\n")
        );
    }

    for host in &hosts {
        fs::remove_file(host.join(&host_name)).expect("remove host marker");
        fs::remove_file(host.join(&guest_name)).expect("remove guest marker");
    }
    println!("P0/P1/P2 mappings verified both ways ({unique})");
}
