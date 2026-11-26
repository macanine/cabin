//! Opt-in account integration test on a disposable Ubuntu VM. Requires the
//! app's cached cloud image, built-in QEMU and a freshly built Linux agentd.
//! All credentials and both CLI executables below are synthetic.
#![cfg(target_os = "macos")]

mod support;

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use vm_core::{
    Config,
    vm::{Boot, Runtime},
};

struct Guest {
    cfg: Config,
    dir: PathBuf,
}
impl Drop for Guest {
    fn drop(&mut self) {
        if let Ok(file) = std::fs::File::create(self.dir.join("stop.log")) {
            Runtime::new(&self.cfg).stop(&Arc::new(Mutex::new(file)), Duration::from_secs(30));
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn run(command: &mut Command) {
    let output = command.output().expect("run command");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_cloud_file(path: &str, content: &str, mode: &str) -> String {
    let indented = content
        .lines()
        .map(|line| format!("      {line}\n"))
        .collect::<String>();
    format!(
        "  - path: {path}\n    owner: dev:dev\n    defer: true\n    permissions: '{mode}'\n    content: |\n{indented}"
    )
}

struct Rpc {
    child: Child,
    input: ChildStdin,
    lines: mpsc::Receiver<String>,
    next_id: u64,
}
impl Rpc {
    fn start(runtime: &Runtime) -> Self {
        let argv = vm_core::agentd::connect_argv(runtime).unwrap();
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout)
                .lines()
                .map_while(std::result::Result::ok)
            {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let hello: Value = serde_json::from_str(
            &lines
                .recv_timeout(Duration::from_secs(30))
                .expect("guest hello"),
        )
        .unwrap();
        assert!(hello["hello"]["build"].as_str().is_some());
        Self {
            child,
            input,
            lines,
            next_id: 1,
        }
    }
    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        writeln!(
            self.input,
            "{}",
            json!({"id":id,"method":method,"params":params})
        )
        .unwrap();
        self.input.flush().unwrap();
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(40))
                .expect("guest RPC response");
            let response: Value = serde_json::from_str(&line).unwrap();
            if response["id"] != id {
                continue;
            }
            assert!(
                response.get("error").is_none(),
                "{method}: {}",
                response["error"]
            );
            return response["result"].clone();
        }
    }
}
impl Drop for Rpc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "boots a disposable VM using an explicit cloud image and test-only CLI stubs"]
fn account_import_export_and_limits_over_guest_ssh() {
    let image = PathBuf::from(std::env::var_os("CABIN_TEST_IMAGE").expect("set CABIN_TEST_IMAGE"));
    let engine_home = PathBuf::from(
        std::env::var_os("CABIN_TEST_ENGINE_HOME").expect("set CABIN_TEST_ENGINE_HOME"),
    );
    let agentd =
        PathBuf::from(std::env::var_os("CABIN_TEST_AGENTD").expect("set CABIN_TEST_AGENTD"));
    assert!(image.is_file() && agentd.is_file());
    let dir = PathBuf::from(format!("/tmp/va-accounts-{}", std::process::id()));
    assert!(!dir.exists(), "refusing to reuse a test directory");
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let home = dir.join("vm");
    let cfg = vm_core::build_config(&json!({"home":home,"name":"agent","username":"dev","hostname":"accounttest","network_mode":"user","ssh_port":port,"memory_mb":1536,"cpus":2,"disk_size":"10G"}), root).unwrap();
    std::fs::create_dir_all(cfg.instance_dir()).unwrap();
    let _guest = Guest {
        cfg: cfg.clone(),
        dir: dir.clone(),
    };
    run(Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(cfg.ssh_key_path()));
    let public_key = std::fs::read_to_string(cfg.ssh_pub_path()).unwrap();
    let seed = dir.join("seed-data");
    std::fs::create_dir_all(&seed).unwrap();
    let mut user_data = format!(
        "#cloud-config\nusers:\n  - name: dev\n    sudo: ALL=(ALL) NOPASSWD:ALL\n    groups: [sudo]\n    shell: /bin/bash\n    ssh_authorized_keys:\n      - {}\nssh_pwauth: false\npackage_update: false\nwrite_files:\n",
        public_key.trim()
    );
    user_data.push_str(&write_cloud_file(
        "/home/dev/.claude/.credentials.json",
        r#"{"claudeAiOauth":{"accessToken":"test-claude-token"}}"#,
        "0600",
    ));
    user_data.push_str(&write_cloud_file(
        "/home/dev/.claude.json",
        r#"{"oauthAccount":{"emailAddress":"claude@example.test"}}"#,
        "0600",
    ));
    std::fs::write(seed.join("user-data"), user_data).unwrap();
    std::fs::write(
        seed.join("meta-data"),
        "instance-id: accounts-test-1\nlocal-hostname: accounttest\n",
    )
    .unwrap();
    std::fs::write(
        seed.join("network-config"),
        serde_json::to_string_pretty(&vm_core::steps::seed::build_network_config(&cfg).unwrap())
            .unwrap(),
    )
    .unwrap();
    vm_core::platform::build_seed_iso(
        &seed,
        &cfg.seed_path(),
        &["user-data", "meta-data", "network-config"],
    )
    .unwrap();
    let engine_path = vm_core::qemu_engine::root(&home);
    std::fs::create_dir_all(engine_path.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(vm_core::qemu_engine::root(&engine_home), engine_path).unwrap();
    let engine = vm_core::qemu_engine::require(&home).unwrap();
    let base = dir.join("test-base.img");
    std::fs::copy(&image, &base).unwrap();
    run(Command::new(&engine.qemu_img)
        .args(["create", "-f", "qcow2", "-F", "qcow2", "-b"])
        .arg(&base)
        .arg(cfg.disk_path()));
    run(Command::new(&engine.qemu_img)
        .arg("resize")
        .arg(cfg.disk_path())
        .arg("10G"));
    std::fs::copy(engine.firmware_vars, cfg.efi_vars_path()).unwrap();
    cfg.save_instance_meta().unwrap();
    let log = Arc::new(Mutex::new(
        std::fs::File::create(dir.join("runtime.log")).unwrap(),
    ));
    let runtime = Runtime::new(&cfg);
    runtime
        .start(&log, Boot::new(Duration::from_secs(240)))
        .expect("boot isolated VM");
    let (code, output) =
        runtime.ssh_capture("cloud-init status --wait --long", Duration::from_secs(180));
    assert_eq!(code, 0, "cloud-init: {output}");
    vm_core::agentd::deploy(&runtime, &agentd).expect("deploy test agentd");
    let helper = support::build_guest_helper(&dir);
    let ssh = runtime.ssh_argv(false).unwrap();
    support::install_guest_helper(Command::new(&ssh[0]).args(&ssh[1..]), &helper);
    let (code, output) = runtime.ssh_capture(
        "ln -sf \"$HOME/.local/bin/guest-test-helper\" \"$HOME/.local/bin/codex\"; ln -sf \"$HOME/.local/bin/guest-test-helper\" \"$HOME/.local/bin/claude\"",
        Duration::from_secs(20),
    );
    assert_eq!(code, 0, "{output}");
    let mut rpc = Rpc::start(&runtime);
    let agents = rpc.call("agents_refresh", json!({}));
    let claude = agents
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["kind"] == "claude")
        .unwrap();
    assert_eq!(claude["authenticated"], true);
    let login = &claude["claudeLogin"];
    assert_eq!(login["state"], "active");
    let account = &login["account"];
    assert_eq!(account["email"], "claude@example.test");
    assert_eq!(account["verified"], true);
    let account_id = account["id"]
        .as_str()
        .expect("identified Claude subscription seat");
    let usage = rpc.call("claude_usage", json!({ "accountId": account_id }));
    assert_eq!(usage["windows"][0]["usedPercent"], 37.5);
    assert_eq!(usage["windows"][0]["resetsAt"], 1_900_000_000_000i64);
    assert_eq!(usage["windows"][1]["usedPercent"], 18.0);

    let first = json!({"type":"codex","email":"first@example.test","plan_type":"plus","account_id":"first-id","access_token":"test-access-one","refresh_token":"test-refresh-one"});
    let imported = rpc.call(
        "codex_import_account",
        json!({"contents":first.to_string(),"activate":true}),
    );
    let id = imported["activeAccountId"].as_str().unwrap().to_string();
    assert_eq!(imported["accounts"][0]["email"], "first@example.test");
    let limits = rpc.call("codex_refresh_quota", json!({}));
    assert_eq!(limits["quota"]["windows"][0]["usedPercent"], 42);
    assert_eq!(
        limits["quota"]["windows"][0]["resetsAt"],
        1_900_000_000_000i64
    );
    assert_eq!(limits["quota"]["windows"][1]["usedPercent"], 23);
    for format in ["native", "cliproxyapi", "sub2api"] {
        let exported = rpc.call(
            "codex_export_account",
            json!({"accountId":id,"format":format}),
        );
        let exported = exported.as_str().unwrap();
        let round_trip = rpc.call(
            "codex_import_account",
            json!({"contents":exported,"activate":false}),
        );
        assert_eq!(
            round_trip["accounts"].as_array().unwrap().len(),
            1,
            "{format} import should identify the same account"
        );
    }
    let second = json!({"type":"codex","email":"second@example.test","plan_type":"pro","account_id":"second-id","access_token":"test-access-two","refresh_token":"test-refresh-two"});
    let imported = rpc.call(
        "codex_import_account",
        json!({"contents":second.to_string(),"activate":false}),
    );
    assert_eq!(imported["accounts"].as_array().unwrap().len(), 2);
    assert_eq!(imported["activeAccountId"], id);
    let second_id = imported["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["email"] == "second@example.test")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let switched = rpc.call("codex_switch_account", json!({"accountId":second_id}));
    assert_eq!(switched["activeAccountId"], second_id);
    let second_limits = rpc.call("codex_refresh_quota", json!({}));
    assert_eq!(second_limits["activeAccountId"], second_id);
    assert_eq!(second_limits["quota"]["planType"], "pro");
    assert_eq!(second_limits["quota"]["windows"][0]["usedPercent"], 71);
    println!(
        "PASS: isolated VM, SSH agentd handshake, Claude status and quotas, Codex import/export/round-trip/switch and quotas"
    );
    // A manual UI run may attach to this same disposable machine. The caller
    // owns the rendezvous directory and releases the VM by creating `done`.
    if let Some(ready_dir) = std::env::var_os("CABIN_TEST_READY_DIR") {
        let ready_dir = PathBuf::from(ready_dir);
        assert!(ready_dir.is_dir());
        std::fs::write(ready_dir.join("ready"), dir.display().to_string()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(600);
        while !ready_dir.join("done").exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200));
        }
        assert!(
            ready_dir.join("done").exists(),
            "timed out waiting for the UI run"
        );
    }
}
