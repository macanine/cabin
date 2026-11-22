//! Explicit live test for the single Chrome process contract.
//!
//! Run with:
//!   VMCTL_E2E_HOME=/tmp/example VMCTL_E2E_INSTANCE=workspace \
//!     cargo test --test browser_live -- --ignored --nocapture

mod support;

use std::collections::HashMap;
use std::path::PathBuf;

use vm_core::shell;
use vm_core::vm::Runtime;

#[test]
#[ignore = "requires a running VM with a Sway desktop and Chrome"]
fn repeated_open_keeps_one_browser_instance() {
    let home = PathBuf::from(
        std::env::var("VMCTL_E2E_HOME").expect("VMCTL_E2E_HOME must point to the vmctl home"),
    );
    let name =
        std::env::var("VMCTL_E2E_INSTANCE").expect("VMCTL_E2E_INSTANCE must name the test VM");
    let status = shell::browser_status(&home, &name).expect("read Browser status");
    assert_eq!(status["vm_running"], true);
    assert_eq!(status["desktop_applied"], true);
    assert_eq!(status["chrome_installed"], true);

    for url in ["about:blank", "about:blank"] {
        let opened = shell::browser_open(&home, &name, url).expect("open Chrome");
        assert_eq!(opened.code, 0, "{}", opened.output);
    }
    assert!(shell::browser_open(&home, &name, "file:///etc/passwd").is_err());

    let config = vm_core::load_instance_config(&vm_core::default_project_root(), &home, &name)
        .expect("load instance");
    let runtime = Runtime::new(&config);
    let helper_dir = home.join("test-helper");
    std::fs::create_dir_all(&helper_dir).unwrap();
    let helper = support::build_guest_helper(&helper_dir);
    let ssh = runtime.ssh_argv(false).unwrap();
    support::install_guest_helper(std::process::Command::new(&ssh[0]).args(&ssh[1..]), &helper);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let counts = loop {
        let counts = guest_browser_counts(&runtime, config.browser_enabled());
        if !config.browser_enabled()
            || (counts.get("page_target").copied() == Some(1)
                && counts.get("ready").copied() == Some(1))
        {
            break counts;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Browser did not settle: {counts:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(250));
    };
    assert_eq!(
        counts.get("chrome_main").copied(),
        Some(1),
        "Chrome main process count: {counts:?}"
    );
    if config.browser_enabled() {
        assert_eq!(counts.get("wrapper").copied(), Some(1), "{counts:?}");
        assert_eq!(counts.get("controller").copied(), Some(1), "{counts:?}");
        assert_eq!(counts.get("page_target").copied(), Some(1), "{counts:?}");
        assert_eq!(counts.get("ready").copied(), Some(1), "{counts:?}");
    }
    println!("{}", opened_text(&status));
}

fn guest_browser_counts(runtime: &Runtime, browser_enabled: bool) -> HashMap<String, u64> {
    let page_probe = if browser_enabled {
        r#""$HOME/.local/bin/guest-test-helper" network pages 9222"#
    } else {
        "echo 0"
    };
    let ready_probe = if browser_enabled {
        r#""$HOME/.local/bin/guest-test-helper" network status "${XDG_RUNTIME_DIR:-/tmp}/vmctl-fp-status.json" | grep -q "\"ready\": *true" && echo 1 || echo 0"#
    } else {
        "echo 0"
    };
    let script = format!(
        r#"set -eu
uid=$(id -u)
wrapper=$(pgrep -u "$uid" -f '^/bin/sh /home/[^ ]+/.config/sway/start-chrome[.]sh$' 2>/dev/null | wc -l | tr -d ' ')
controller=$(pgrep -u "$uid" -f 'cabin/cabind browser run' 2>/dev/null | wc -l | tr -d ' ')
chrome_main=$(ps -u "$uid" -o args= | awk '$1 == "/opt/google/chrome/chrome" && $0 !~ / --type=/ {{ count++ }} END {{ print count + 0 }}')
page_target=$({page_probe} 2>/dev/null) || page_target=0
ready=$({ready_probe} 2>/dev/null) || ready=0
printf 'wrapper=%s\ncontroller=%s\nchrome_main=%s\npage_target=%s\nready=%s\n' \
  "$wrapper" "$controller" "$chrome_main" "$page_target" "$ready"
"#
    );
    let (code, output) = runtime.ssh_capture(&script, std::time::Duration::from_secs(15));
    eprintln!("browser count probe:\n{output}");
    assert_eq!(code, 0, "{output}");
    output
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            Some((key.to_string(), value.trim().parse().ok()?))
        })
        .collect()
}

fn opened_text(status: &serde_json::Value) -> String {
    format!(
        "{} on {}",
        status["chrome_version"].as_str().unwrap_or("Chrome"),
        status["desktop_env"].as_str().unwrap_or("desktop")
    )
}
