use super::transport::with_remote_timeout;
use super::*;
use agent_core::sync::GitHandling;
use std::process::Command;
#[cfg(unix)]
use std::process::Stdio;

#[cfg(any(target_os = "macos", windows))]
mod vm_live;

#[test]
fn a_full_watcher_queue_requests_a_rescan() {
    let (sender, receiver) = mpsc::sync_channel(1);
    let queue = Kicks {
        sender,
        rescan: Arc::new(AtomicBool::new(false)),
    };
    queue.send(Kick::Scope(vec!["first".into()])).unwrap();
    queue.send(Kick::Scope(vec!["discarded".into()])).unwrap();
    assert!(matches!(receiver.try_recv(), Ok(Kick::Scope(paths)) if paths == ["first"]));
    assert!(queue.take_rescan());
    assert!(!queue.take_rescan());
    drop(receiver);
    assert!(queue.send(Kick::Full).is_err());
}

#[test]
fn corrupt_base_stops_sync_instead_of_losing_history() {
    let home = tempfile::tempdir().unwrap();
    let manager = SyncManager::open(home.path().join("sync"));
    let path = manager.base_path("project");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"broken base").unwrap();
    assert!(
        manager
            .load_base("project")
            .unwrap_err()
            .contains("Could not read the sync base")
    );
    assert_eq!(std::fs::read(path).unwrap(), b"broken base");
}

#[test]
fn corrupt_project_registry_cannot_be_replaced_or_erased() {
    let root = tempfile::tempdir().unwrap();
    let sync = root.path().join("sync");
    let project = root.path().join("project");
    std::fs::create_dir(&sync).unwrap();
    std::fs::create_dir(&project).unwrap();
    let registry = sync.join("projects.json");
    std::fs::write(&registry, b"truncated registry").unwrap();
    let manager = SyncManager::open(sync);
    let error = manager
        .create(SyncProjectInput {
            workspace_id: None,
            name: "example".into(),
            host_path: project.to_string_lossy().into_owned(),
            guest_path: "~/projects/example".into(),
            mode: SyncMode::Safe,
            rules: SyncRules::default(),
        })
        .unwrap_err();
    assert!(error.contains("project registry"), "{error}");
    assert!(manager.overlaps_host_path(root.path()).is_err());
    assert!(
        manager
            .reset_storage::<()>(|| panic!("erased project state"))
            .is_err()
    );
    assert_eq!(std::fs::read(registry).unwrap(), b"truncated registry");
}

#[test]
fn removing_an_unknown_project_leaves_other_state_alone() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("other-state");
    std::fs::create_dir(&state).unwrap();
    std::fs::write(state.join("keep"), b"other app state").unwrap();
    let manager = SyncManager::open(root.path().to_path_buf());
    assert!(manager.remove("other-state").is_err());
    assert_eq!(
        std::fs::read(state.join("keep")).unwrap(),
        b"other app state"
    );
}

#[cfg(unix)]
#[test]
fn short_sync_commands_share_one_ssh_transport() {
    let dir = tempfile::tempdir().unwrap();
    let ssh = reuse_ssh(vec!["ssh".into(), "dev@example".into()], dir.path());
    assert!(ssh.iter().any(|arg| arg == "ControlMaster=auto"));
    assert!(
        ssh.iter()
            .any(|arg| arg.contains("ControlPath=") && arg.contains("cabin-sync-"))
    );
    let local = reuse_ssh(vec!["sh".into(), "-c".into(), "true".into()], dir.path());
    assert_eq!(local, ["sh", "-c", "true"]);
}

#[cfg(unix)]
#[test]
fn a_hung_guest_command_is_stopped_at_the_deadline() {
    let mut command = Command::new("sh");
    command
        .args(["-c", "sleep 30"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let started = Instant::now();
    let error = with_remote_timeout(
        command,
        "Scanning",
        Duration::from_millis(200),
        None,
        |_| Ok(()),
    )
    .unwrap_err();
    assert!(error.contains("did not finish"), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the deadline is the bound"
    );
}

#[cfg(unix)]
#[test]
fn a_guest_command_with_inherited_pipes_still_obeys_the_deadline() {
    let mut command = Command::new("sh");
    command
        .args(["-c", "sleep 30 & exit 0"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let started = Instant::now();
    let error = with_remote_timeout(
        command,
        "Scanning",
        Duration::from_millis(200),
        None,
        |_| Ok(()),
    )
    .unwrap_err();
    assert!(error.contains("did not finish"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[cfg(windows)]
#[test]
fn a_windows_guest_command_reaps_descendants_with_inherited_pipes() {
    let scratch = tempfile::tempdir().unwrap();
    let marker = scratch.path().join("descendant.pid");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--ignored",
            "--exact",
            "sync::tests::windows_inherited_pipe_child",
        ])
        .env("CABIN_TEST_PIPE_PID", &marker);
    let started = Instant::now();
    let error = with_remote_timeout(
        command,
        "Scanning",
        Duration::from_secs(2),
        None,
        |_| Ok(()),
    )
    .unwrap_err();
    assert!(error.contains("did not finish"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(6));
    let pid = std::fs::read_to_string(&marker).unwrap().parse().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while vm_core::util::pid_alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !vm_core::util::pid_alive(pid),
        "descendant {pid} survived the deadline"
    );
}

#[cfg(windows)]
#[test]
#[ignore = "subprocess fixture for the Windows sync timeout regression"]
#[allow(
    clippy::zombie_processes,
    reason = "the fixture must exit while its child holds the pipes"
)]
fn windows_inherited_pipe_child() {
    let Some(marker) = std::env::var_os("CABIN_TEST_PIPE_PID") else {
        return;
    };
    if std::env::var_os("CABIN_TEST_PIPE_HOLD").is_some() {
        std::thread::sleep(Duration::from_secs(10));
        return;
    }
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "sync::tests::windows_inherited_pipe_child",
        ])
        .env("CABIN_TEST_PIPE_HOLD", "1")
        .spawn()
        .unwrap();
    std::fs::write(marker, child.id().to_string()).unwrap();
}

#[cfg(unix)]
#[test]
fn cancelling_native_sync_closes_a_stalled_command() {
    let mut command = Command::new("sh");
    command
        .args(["-c", "sleep 30"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let cancelled = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    let error = with_remote_timeout(
        command,
        "Scanning",
        Duration::from_secs(30),
        Some(&cancelled),
        |_| {
            cancelled.store(true, Ordering::Relaxed);
            Ok(())
        },
    )
    .unwrap_err();
    assert!(error.contains("cancelled"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[cfg(unix)]
#[test]
fn native_handoffs_surface_partial_transfers_and_release_the_lease() {
    let root = tempfile::tempdir().unwrap();
    let host = root.path().join("project");
    std::fs::create_dir(&host).unwrap();
    std::fs::write(host.join("notes.txt"), b"keep both versions").unwrap();
    let manager = SyncManager::open(root.path().join("sync"));
    let view = manager
        .create(SyncProjectInput {
            workspace_id: None,
            name: "example".into(),
            host_path: host.to_string_lossy().into_owned(),
            guest_path: "~/projects/example".into(),
            mode: SyncMode::TwoWay,
            rules: SyncRules {
                git: GitHandling::Skip,
                ..SyncRules::default()
            },
        })
        .unwrap();
    let id = view.project.id;
    let script = r#"
        case "$0" in
                *' sync scan '*) IFS= read -r request; printf 'VASCAN2\000\000\000\000\000';;
            *' sync apply '*) cat >/dev/null; printf '%s' '{"written":0,"deleted":0,"failed":[["notes.txt","write failed"]]}';;
            *' sync watch '*) :;;
            *) exit 10;;
        esac"#;
    *manager.native.write_lock() = Some((
        id.clone(),
        GuestTransport::Ssh(vec!["sh".into(), "-c".into(), script.into()]),
    ));
    let (sender, receiver) = mpsc::channel();
    manager.spawn_session(&id, Some(sender)).unwrap();
    let initial = receiver
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap_err();
    assert!(initial.contains("notes.txt: write failed"), "{initial}");
    let final_error = manager.finish_native(&id, true).unwrap_err();
    assert!(
        final_error.contains("notes.txt: write failed"),
        "{final_error}"
    );
    assert_eq!(manager.active_native_project(), None);
    let status = manager.status(&id);
    assert_eq!(status.state, SyncState::Offline);
    assert_eq!(status.pushed, 0);
    assert!(status.conflicts.is_empty());
}

fn scan_from_script(script: &str, rules: &SyncRules) -> Result<Vec<Entry>, String> {
    let ssh = vec!["sh".into(), "-c".into(), script.into()];
    remote_scan(&GuestTransport::Ssh(ssh), "/unused", rules, None)
}

#[test]
fn remote_scan_drains_stderr_while_sending_and_decoding() {
    // Fill stderr before reading a request larger than a pipe, then return
    // an empty binary listing. Sequential pipe handling would deadlock.
    let rules = SyncRules {
        ignore: vec!["x".repeat(256 * 1024)],
        ..SyncRules::default()
    };
    let script = "dd if=/dev/zero bs=1024 count=256 >&2 2>/dev/null; IFS= read -r request; printf 'VASCAN2\\000\\000\\000\\000\\000'";
    assert_eq!(
        scan_from_script(script, &rules).unwrap(),
        Vec::<Entry>::new()
    );
}

#[test]
fn remote_scan_reports_exit_and_bounds_non_utf8_stderr() {
    let script = "IFS= read -r request; printf 'failure: \\377' >&2; dd if=/dev/zero bs=1024 count=128 >&2 2>/dev/null; exit 7";
    let error = scan_from_script(script, &SyncRules::default()).unwrap_err();
    assert!(
        error.contains("Scanning failed in the machine: failure: �"),
        "{error:?}"
    );
    assert!(error.contains("[truncated]"));
    assert!(error.len() < 70 * 1024);
}

#[test]
fn remote_upload_drains_diagnostics_before_sending_archive() {
    let host = tempfile::tempdir().unwrap();
    std::fs::write(host.path().join("large.txt"), vec![b'x'; 512 * 1024]).unwrap();
    let script = r#"dd if=/dev/zero bs=1024 count=256 >&2 2>/dev/null; cat >/dev/null; printf '%s' '{"written":1,"deleted":0,"failed":[]}'"#;
    let ssh = vec!["sh".into(), "-c".into(), script.into()];
    let report = remote_apply(
        &GuestTransport::Ssh(ssh),
        "/unused",
        host.path(),
        &[],
        &["large.txt".into()],
        None,
    )
    .unwrap();
    assert_eq!(report.written, 1);
}

#[test]
fn remote_download_keeps_the_exit_error_after_large_diagnostics() {
    let host = tempfile::tempdir().unwrap();
    let script = "IFS= read -r request; printf 'pack failed: \\377' >&2; dd if=/dev/zero bs=1024 count=256 >&2 2>/dev/null; exit 7";
    let ssh = vec!["sh".into(), "-c".into(), script.into()];
    let error = pull_into(
        &GuestTransport::Ssh(ssh),
        "/unused",
        host.path(),
        &[],
        &["file.txt".into()],
        None,
    )
    .unwrap_err();
    assert!(error.contains("Reading files failed in the machine: pack failed: �"));
    assert!(error.ends_with("[truncated]"));
    assert!(error.len() < 70 * 1024);
}

#[test]
fn remote_scan_rejects_bad_streams_and_checks_exit_status() {
    let rules = SyncRules::default();
    let bad =
        "IFS= read -r request; printf 'bad data'; dd if=/dev/zero bs=1024 count=128 2>/dev/null";
    assert!(
        scan_from_script(bad, &rules)
            .unwrap_err()
            .contains("file list was unreadable")
    );
    let legacy = "IFS= read -r request; printf 'VASCAN1\\000\\000\\000\\000\\000'";
    assert!(
        scan_from_script(legacy, &rules)
            .unwrap_err()
            .contains("machine must be updated")
    );
    let trailing = "IFS= read -r request; printf 'VASCAN2\\000\\000\\000\\000\\000extra'";
    assert!(
        scan_from_script(trailing, &rules)
            .unwrap_err()
            .contains("trailing data")
    );
    let failed = "IFS= read -r request; printf 'VASCAN2\\000\\000\\000\\000\\000'; printf 'scan failed' >&2; exit 9";
    assert!(
        scan_from_script(failed, &rules)
            .unwrap_err()
            .contains("scan failed")
    );
    let early = "exit 9";
    assert!(
        scan_from_script(early, &rules)
            .unwrap_err()
            .contains("Scanning failed in the machine")
    );
}

#[test]
fn parents_quotes_and_overlaps() {
    assert_eq!(parent_of("a/b/c.rs"), "a/b");
    assert_eq!(parent_of("top"), "");
    assert_eq!(shell_quote("~/it's here"), r"'~/it'\''s here'");
}

#[test]
fn guest_sync_roots_must_be_explicit_folders() {
    for path in [
        "/",
        "~",
        "/.",
        "~/..",
        "/home/../",
        "//home/user",
        "~/app\0",
        "~/app\\test",
    ] {
        assert!(guest_folder(path).is_err(), "accepted {path:?}");
    }
    assert_eq!(
        guest_folder(" /home/user/project/ ").unwrap(),
        "/home/user/project"
    );
    assert_eq!(
        guest_folder("~/projects/my app").unwrap(),
        "~/projects/my app"
    );
}

#[test]
fn projects_persist_and_reject_overlapping_folders() {
    let home = tempfile::tempdir().unwrap();
    let mac = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(mac.path().join("app/inner")).unwrap();
    let manager = SyncManager::open(home.path().join("sync"));
    let input = |path: &Path, guest: &str| SyncProjectInput {
        workspace_id: None,
        name: "app".into(),
        host_path: path.display().to_string(),
        guest_path: guest.into(),
        mode: SyncMode::Safe,
        rules: SyncRules::default(),
    };
    let view = manager
        .create(input(&mac.path().join("app"), "~/projects/app"))
        .unwrap();
    assert_eq!(view.status.state, SyncState::Offline);
    assert!(
        manager
            .create(input(&mac.path().join("app/inner"), "~/projects/other"))
            .unwrap_err()
            .contains("overlaps")
    );
    assert!(
        manager
            .create(input(mac.path(), "~/projects/app/sub"))
            .unwrap_err()
            .contains("overlaps")
    );
    assert!(manager.create(input(mac.path(), "relative")).is_err());

    let again = SyncManager::open(home.path().join("sync"));
    assert_eq!(again.list().len(), 1);
    let paused = again
        .update(&view.project.id, Some(SyncMode::TwoWay), None, Some(true))
        .unwrap();
    assert_eq!(
        (paused.project.mode, paused.status.state),
        (SyncMode::TwoWay, SyncState::Paused)
    );
    again.remove(&view.project.id).unwrap();
    assert!(
        SyncManager::open(home.path().join("sync"))
            .list()
            .is_empty()
    );
}

#[test]
fn failed_project_save_preserves_live_and_saved_state() {
    let home = tempfile::tempdir().unwrap();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let dir = home.path().join("sync");
    let manager = SyncManager::open(dir.clone());
    let input = |host: &Path, name: &str| SyncProjectInput {
        workspace_id: None,
        name: name.into(),
        host_path: host.display().to_string(),
        guest_path: format!("~/projects/{name}"),
        mode: SyncMode::Safe,
        rules: SyncRules::default(),
    };
    let id = manager
        .create(input(first.path(), "first"))
        .unwrap()
        .project
        .id;
    let base = manager.base_path(&id);
    std::fs::create_dir_all(base.parent().unwrap()).unwrap();
    std::fs::write(&base, b"existing sync history").unwrap();
    let registry = dir.join("projects.json");
    let backup = dir.join("saved-projects.json");
    std::fs::rename(&registry, &backup).unwrap();
    std::fs::create_dir(&registry).unwrap();

    assert!(manager.create(input(second.path(), "second")).is_err());
    assert!(
        manager
            .update(&id, Some(SyncMode::TwoWay), None, Some(true))
            .is_err()
    );
    assert!(manager.remove(&id).is_err());

    std::fs::remove_dir(&registry).unwrap();
    std::fs::rename(&backup, &registry).unwrap();
    for list in [manager.list(), SyncManager::open(dir).list()] {
        let [view] = list.as_slice() else {
            panic!("expected the original project")
        };
        assert_eq!(view.project.id, id);
        assert_eq!(view.project.mode, SyncMode::Safe);
        assert!(!view.project.paused);
        assert_eq!(view.status.state, SyncState::Offline);
    }
    assert_eq!(std::fs::read(base).unwrap(), b"existing sync history");
}

#[test]
fn reset_refuses_an_active_sync_or_a_mac_folder_inside_the_data_root() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let external = root.path().join("external");
    let nested = data.join("sync/project");
    for path in [&external, &nested] {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join("keep"), "user files").unwrap();
    }
    let sync = SyncManager::open(data.join("sync"));
    let project = |host: &Path, guest: &str| SyncProjectInput {
        workspace_id: None,
        name: guest.into(),
        host_path: host.display().to_string(),
        guest_path: format!("~/projects/{guest}"),
        mode: SyncMode::Safe,
        rules: SyncRules::default(),
    };
    let view = sync.create(project(&external, "external")).unwrap();
    let lock = sync.cycle_lock(&view.project.id);
    let active = lock.lock().unwrap();
    assert!(
        sync.reset_storage::<()>(|| panic!("reset ran during sync"))
            .unwrap_err()
            .to_string()
            .contains("finishing")
    );
    drop(active);
    sync.create(project(&nested, "nested")).unwrap();
    assert!(
        sync.reset_storage::<()>(|| panic!("reset would erase a Mac folder"))
            .unwrap_err()
            .to_string()
            .contains("inside the app data directory")
    );
    assert!(nested.join("keep").is_file());
    assert_eq!(sync.list().len(), 2);
}

/// The real cycle against the real `cabind sync`, with the SSH hop
/// replaced by a local shell whose HOME holds the binary.
#[cfg(unix)]
fn local_guest() -> Option<(tempfile::TempDir, Vec<String>)> {
    let configured = std::env::var_os("CABIND_TEST_BIN");
    let binary = configured.as_ref().map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/target/debug/cabind"),
        PathBuf::from,
    );
    // It must exist and run on this host (a shared target folder may hold
    // another platform's build).
    let runs = Command::new(&binary)
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success());
    if !runs {
        assert!(
            configured.is_none(),
            "CABIND_TEST_BIN is not a runnable cabind: {}",
            binary.display()
        );
        eprintln!(
            "skipping: no runnable cabind (cargo build --bin cabind, or set CABIND_TEST_BIN)"
        );
        return None;
    }
    let home = tempfile::tempdir().unwrap();
    let bin = home.path().join(vm_core::agentd::REMOTE_BIN);
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(binary.canonicalize().unwrap(), &bin).unwrap();
    let ssh = vec![
        "env".into(),
        format!("HOME={}", home.path().display()),
        "sh".into(),
        "-c".into(),
    ];
    Some((home, ssh))
}

/// `cabind` runs in the Linux guest; its end-to-end tests need a Unix shell.
#[cfg(not(unix))]
fn local_guest() -> Option<(tempfile::TempDir, Vec<String>)> {
    eprintln!("skipping: the end-to-end sync tests need a Unix shell");
    None
}

fn write(root: &Path, rel: &str, body: &str, secs: u64) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, body).unwrap();
    let time = std::time::UNIX_EPOCH + Duration::from_secs(secs);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(time))
        .unwrap();
}

fn read(root: &Path, rel: &str) -> Option<String> {
    std::fs::read_to_string(root.join(rel)).ok()
}

fn setup(
    mode: SyncMode,
) -> Option<(
    tempfile::TempDir,
    tempfile::TempDir,
    Arc<SyncManager>,
    SyncProject,
    PathBuf,
    PathBuf,
)> {
    let (guest_home, ssh) = local_guest()?;
    let state = tempfile::tempdir().unwrap();
    let mac = state.path().join("mac");
    std::fs::create_dir_all(&mac).unwrap();
    write(&mac, "src/main.rs", "v1", 1_000);
    write(&mac, ".gitignore", "target/\n", 1_000);
    write(&mac, "target/big.o", "build output", 1_000);
    write(&mac, ".git/HEAD", "ref: refs/heads/main", 1_000);
    let manager = SyncManager::open(state.path().join("sync"));
    let view = manager
        .create(SyncProjectInput {
            workspace_id: None,
            name: "app".into(),
            host_path: mac.display().to_string(),
            guest_path: "~/projects/app".into(),
            mode,
            rules: SyncRules::default(),
        })
        .unwrap();
    *manager.guest.write().unwrap() = Some(GuestTransport::Ssh(ssh));
    let vm = guest_home.path().join("projects/app");
    let mac = PathBuf::from(&view.project.host_path);
    Some((guest_home, state, manager, view.project, mac, vm))
}

#[test]
fn safe_mode_end_to_end() {
    let Some((_g, _s, manager, project, mac, vm)) = setup(SyncMode::Safe) else {
        return;
    };
    manager.first_cycle(&project).unwrap();
    assert_eq!(read(&vm, "src/main.rs").as_deref(), Some("v1"));
    assert_eq!(
        read(&vm, ".git/HEAD").as_deref(),
        Some("ref: refs/heads/main"),
        ".git is copied once"
    );
    assert!(
        read(&vm, "target/big.o").is_none(),
        "gitignored output stays on the Mac"
    );
    let project = manager.project(&project.id).unwrap();
    assert!(project.git_copied);

    // The Mac edits; an agent adds a file in the machine.
    write(&mac, "src/main.rs", "v2", 2_000);
    write(&vm, "src/agent.rs", "mine", 2_000);
    manager.cycle(&project, Some(vec!["src".into()])).unwrap();
    assert_eq!(read(&vm, "src/main.rs").as_deref(), Some("v2"));
    assert_eq!(
        read(&vm, "src/agent.rs").as_deref(),
        Some("mine"),
        "machine-only files are kept"
    );
    assert!(
        read(&mac, "src/agent.rs").is_none(),
        "nothing flows back in Safe mode"
    );

    // Both edit the same file: a conflict, nothing overwritten.
    write(&mac, "src/main.rs", "mac v3", 3_000);
    write(&vm, "src/main.rs", "agent v3", 3_001);
    manager.cycle(&project, None).unwrap();
    assert_eq!(read(&vm, "src/main.rs").as_deref(), Some("agent v3"));
    assert_eq!(manager.status(&project.id).conflicts, ["src/main.rs"]);

    // Keep the Mac's version.
    manager.resolve(&project.id, "src/main.rs", true).unwrap();
    assert_eq!(read(&vm, "src/main.rs").as_deref(), Some("mac v3"));
    assert!(manager.status(&project.id).conflicts.is_empty());
    manager.cycle(&project, None).unwrap();
    assert!(
        manager.status(&project.id).conflicts.is_empty(),
        "a resolved conflict stays resolved"
    );
}

#[test]
fn mirror_mode_end_to_end() {
    let Some((_g, _s, manager, project, mac, vm)) = setup(SyncMode::Mirror) else {
        return;
    };
    manager.first_cycle(&project).unwrap();
    write(&vm, "src/main.rs", "agent edit", 5_000);
    write(&vm, "stray.txt", "x", 5_000);
    std::fs::remove_file(mac.join("src/main.rs")).unwrap();
    write(&mac, "src/lib.rs", "lib", 5_000);
    manager.cycle(&project, None).unwrap();
    assert!(read(&vm, "src/main.rs").is_none());
    assert!(read(&vm, "stray.txt").is_none());
    assert_eq!(read(&vm, "src/lib.rs").as_deref(), Some("lib"));
}

#[test]
fn two_way_mode_end_to_end() {
    let Some((_g, _s, manager, project, mac, vm)) = setup(SyncMode::TwoWay) else {
        return;
    };
    manager.first_cycle(&project).unwrap();
    write(&vm, "src/main.rs", "agent v2", 2_000);
    write(&vm, "src/new/agent.rs", "new", 2_000);
    write(&mac, "README.md", "readme", 2_000);
    manager.cycle(&project, None).unwrap();
    assert_eq!(read(&mac, "src/main.rs").as_deref(), Some("agent v2"));
    assert_eq!(read(&mac, "src/new/agent.rs").as_deref(), Some("new"));
    assert_eq!(read(&vm, "README.md").as_deref(), Some("readme"));
    // A deletion in the machine reaches the Mac; nothing ping-pongs.
    std::fs::remove_file(vm.join("src/new/agent.rs")).unwrap();
    manager.cycle(&project, None).unwrap();
    assert!(read(&mac, "src/new/agent.rs").is_none());
    let before = manager.status(&project.id);
    manager.cycle(&project, None).unwrap();
    let after = manager.status(&project.id);
    assert_eq!(
        (before.pushed, before.pulled),
        (after.pushed, after.pulled),
        "a settled tree needs nothing"
    );
    // The machine's own git history is never overwritten after the copy.
    write(&vm, ".git/HEAD", "ref: refs/heads/agent", 9_000);
    manager.cycle(&project, None).unwrap();
    assert_eq!(
        read(&mac, ".git/HEAD").as_deref(),
        Some("ref: refs/heads/main")
    );
}

#[test]
fn equal_metadata_divergence_remains_a_conflict_until_resolved() {
    let Some((_guest, _state, manager, project, host, guest)) = setup(SyncMode::TwoWay) else {
        return;
    };
    let path = "src/main.rs";
    manager
        .first_cycle(&project)
        .unwrap()
        .require_complete()
        .unwrap();
    write(&host, path, "abc", 2_000);
    write(&guest, path, "xyz", 2_000);
    assert!(
        manager
            .cycle(&project, None)
            .unwrap()
            .require_complete()
            .is_err()
    );
    write(&host, path, "abcd", 2_001);
    assert!(
        manager
            .cycle(&project, None)
            .unwrap()
            .require_complete()
            .is_err()
    );
    assert_eq!(read(&guest, path).as_deref(), Some("xyz"));
    manager.resolve(&project.id, path, false).unwrap();
    assert_eq!(read(&host, path).as_deref(), Some("xyz"));
    assert!(manager.status(&project.id).conflicts.is_empty());
}

#[test]
fn ignore_rules_cannot_turn_a_present_file_into_a_deletion() {
    for ignore_on_host in [true, false] {
        let Some((_guest, _state, manager, project, host, guest)) = setup(SyncMode::TwoWay) else {
            return;
        };
        manager
            .first_cycle(&project)
            .unwrap()
            .require_complete()
            .unwrap();
        let side = if ignore_on_host { &host } else { &guest };
        let other = if ignore_on_host { &guest } else { &host };
        write(side, ".gitignore", "target/\nsrc/main.rs\n", 2_000);
        write(side, "notes.txt", "also sync this", 2_000);
        manager
            .cycle(&project, None)
            .unwrap()
            .require_complete()
            .unwrap();
        assert_eq!(read(other, "notes.txt").as_deref(), Some("also sync this"));
        assert_eq!(
            read(other, ".gitignore").as_deref(),
            Some("target/\nsrc/main.rs\n")
        );
        assert_eq!(read(&host, "src/main.rs").as_deref(), Some("v1"));
        assert_eq!(read(&guest, "src/main.rs").as_deref(), Some("v1"));
    }
}

#[cfg(unix)]
#[test]
fn a_failed_pull_preserves_an_acknowledged_push_base() {
    let Some((guest_home, _state, manager, project, host, guest)) = setup(SyncMode::TwoWay) else {
        return;
    };
    manager
        .first_cycle(&project)
        .unwrap()
        .require_complete()
        .unwrap();
    let normal = manager.guest().unwrap();
    let path = "src/main.rs";
    write(&host, path, "v2", 2_000);
    write(&guest, "notes.txt", "guest edit", 2_000);
    *manager.guest.write_lock() = Some(GuestTransport::Ssh(vec![
        "env".into(),
        format!("HOME={}", guest_home.path().display()),
        "sh".into(),
        "-c".into(),
        r#"case "$0" in *' sync pack '*) echo 'simulated lost pull' >&2; exit 7;; esac; exec sh -c "$0""#.into(),
    ]));
    let Err(error) = manager.cycle(&project, None) else {
        panic!("the pull must fail after the push");
    };
    assert!(error.contains("simulated lost pull"), "{error}");
    assert_eq!(read(&guest, path).as_deref(), Some("v2"));

    write(&host, path, "v1", 3_000);
    *manager.guest.write_lock() = Some(normal);
    manager
        .cycle(&project, None)
        .unwrap()
        .require_complete()
        .unwrap();
    assert_eq!(read(&host, path).as_deref(), Some("v1"));
    assert_eq!(read(&guest, path).as_deref(), Some("v1"));
    assert_eq!(read(&host, "notes.txt").as_deref(), Some("guest edit"));
}

fn bench_throughput(mode: SyncMode, label: &str) {
    let Some((_g, _s, manager, project, mac, _vm)) = setup(mode) else {
        return;
    };
    for dir in 0..200 {
        for file in 0..100 {
            write(
                &mac,
                &format!("src/d{dir}/f{file}.rs"),
                "fn main() { println!(\"hello\"); }\n",
                1_000,
            );
        }
    }
    for big in 0..5 {
        std::fs::write(mac.join(format!("assets-{big}.bin")), vec![7u8; 10 << 20]).unwrap();
    }
    let started = Instant::now();
    manager.first_cycle(&project).unwrap();
    let first = started.elapsed();
    let started = Instant::now();
    manager.cycle(&project, None).unwrap();
    let noop = started.elapsed();
    write(&mac, "src/d7/f3.rs", "changed", 9_000);
    let started = Instant::now();
    manager
        .cycle(&project, Some(vec!["src/d7".into()]))
        .unwrap();
    let one = started.elapsed();
    eprintln!(
        "{label} 20,000 files + 50 MB: first sync {first:?}; full cycle, nothing changed {noop:?}; one edit (scoped) {one:?}"
    );
}

/// Timings on a real tree (run with --ignored --nocapture).
#[test]
#[ignore = "benchmark"]
fn throughput() {
    bench_throughput(SyncMode::TwoWay, "two-way");
}

#[test]
#[ignore = "benchmark"]
fn throughput_safe() {
    bench_throughput(SyncMode::Safe, "safe");
}
