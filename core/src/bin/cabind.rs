//! `cabind`: the Cabin service inside the VM.
//!
//! - `cabind serve`            run the service (systemd user unit)
//! - `cabind connect`          bridge stdin/stdout to the service (used over SSH)
//! - `cabind install-service`  install/refresh the systemd user unit and start it
//! - `cabind sync <scan|pack|apply|watch> <root>`  the guest's half of project sync
//! - `cabind native-display`   handle a single display request from stdin
//! - `cabind browser ...`      run or inspect the managed Chrome controller
//! - `cabind device-probe`     collect guest device evidence as JSON
//! - `cabind dns-serve`        serve DNS through the managed HTTP proxy
//! - `cabind guest-desktop ...` list, launch or inspect desktop apps
//! - `cabind guest-system ...`  query guest environment and validate JSON
//! - `cabind --version`

use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_core::app::App;
use agent_core::paths::AppPaths;

const UNIT: &str = "cabind.service";

/// Unix socket paths are limited to ~108 bytes; fall back to the runtime
/// directory when the data directory is too deep.
fn socket(paths: &AppPaths) -> PathBuf {
    if let Some(path) = std::env::var_os("CABIND_SOCKET") {
        return PathBuf::from(path);
    }
    let preferred = paths.data().join("agentd.sock");
    if preferred.as_os_str().len() < 100 {
        return preferred;
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
    runtime.join(format!("cabind-{user}.sock"))
}

fn systemctl(args: &[&str]) -> bool {
    Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .is_ok_and(|status| status.success())
}

fn install_service(paths: &AppPaths) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let unit_dir = paths.home().join(".config/systemd/user");
    std::fs::create_dir_all(&unit_dir).map_err(|e| e.to_string())?;
    let unit = format!(
        "[Unit]\nDescription=Cabin service (workspaces and agent sessions)\n\n\
         [Service]\nExecStart={} serve\nRestart=on-failure\nRestartSec=2\n\n\
         [Install]\nWantedBy=default.target\n",
        exe.display()
    );
    agent_core::fsx::atomic_write(&unit_dir.join(UNIT), unit.as_bytes(), Some(0o644))
        .map_err(|e| e.to_string())?;
    // Keep the user manager (and our sessions) alive without a login.
    let user = std::env::var("USER").unwrap_or_default();
    if !user.is_empty() {
        let _ = Command::new("sudo")
            .args(["-n", "loginctl", "enable-linger", &user])
            .status();
    }
    if !(systemctl(&["daemon-reload"])
        && systemctl(&["enable", UNIT])
        && systemctl(&["restart", UNIT]))
    {
        return Err("systemctl --user failed to start cabind".into());
    }
    Ok(())
}

fn guest_desktop(args: &[String]) -> ExitCode {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let result: Result<(), String> = match args.first().map(String::as_str) {
        Some("apps") => serde_json::to_writer(
            std::io::stdout().lock(),
            &agent_core::guest_desktop::list_apps(&home),
        )
        .map_err(|error| error.to_string()),
        Some("launch") => args
            .get(1)
            .ok_or_else(|| "guest-desktop launch needs an app id".into())
            .and_then(|id| agent_core::guest_desktop::launch_app(&home, id).map_err(|e| e.to_string())),
        Some("screen") | Some("output-name") | Some("focused") => (|| {
            let mut input = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin().lock(), &mut input)
                .map_err(|error| error.to_string())?;
            match args[0].as_str() {
                "focused" => match agent_core::guest_desktop::chrome_focused(&input) {
                    Ok(true) => Ok(0),
                    Ok(false) => Ok(1),
                    Err(error) => Err(error.to_string()),
                },
                "screen" => {
                    println!("{}", agent_core::guest_desktop::selected_output(&input)
                        .map_err(|e| e.to_string())?.screen());
                    Ok(0)
                }
                _ => {
                    println!("{}", agent_core::guest_desktop::selected_output(&input)
                        .map_err(|e| e.to_string())?.name);
                    Ok(0)
                }
            }
        })()
        .map(|code| {
            if code == 0 {
                Ok(())
            } else {
                std::process::exit(code);
            }
        })
        .unwrap_or_else(Err),
        _ => Err("usage: guest-desktop apps|launch <id>|screen|output-name|focused".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cabind guest-desktop: {error}");
            ExitCode::FAILURE
        }
    }
}

fn guest_system(args: &[String]) -> ExitCode {
    let result: Result<(), String> = match args.first().map(String::as_str) {
        Some("environment") => agent_core::guest_system::environment()
            .map_err(|e| e.to_string())
            .and_then(|environment| {
                serde_json::to_writer(std::io::stdout().lock(), &environment)
                    .map_err(|error| error.to_string())
            }),
        Some("validate-json") => args
            .get(1)
            .ok_or_else(|| "guest-system validate-json needs a path".into())
            .and_then(|path| {
                agent_core::guest_system::validate_json(std::path::Path::new(path))
                    .map_err(|e| e.to_string())
            }),
        _ => Err("usage: guest-system environment|validate-json <path>".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cabind guest-system: {error}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths = AppPaths::from_env();
    match args.first().map(String::as_str) {
        Some("--version") | Some("-V") => {
            println!("cabind {}", agent_core::rpc::VERSION);
            ExitCode::SUCCESS
        }
        Some("serve") => {
            let app = match App::open(paths.clone()) {
                Ok(app) => Arc::new(app),
                Err(error) => {
                    eprintln!("cabind: {error}");
                    return ExitCode::FAILURE;
                }
            };
            app.start_services();
            match agent_core::rpc::serve(app, &socket(&paths)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("cabind: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("connect") => {
            let socket = socket(&paths);
            if !socket.exists() {
                // Start the service on demand (first connection after boot).
                let _ = systemctl(&["start", UNIT]);
                let started = Instant::now();
                while !socket.exists() && started.elapsed() < Duration::from_secs(10) {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            match agent_core::rpc::connect(&socket) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("cabind: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("native-display") => {
            match agent_core::native_display::run(
                &paths,
                std::io::stdin().lock(),
                std::io::stdout().lock(),
            ) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("{error}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("sync") => match agent_core::sync::run_command(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("cabind sync: {error}");
                ExitCode::FAILURE
            }
        },
        Some("browser") => match agent_core::browser::main(&args[1..]) {
            Ok(code) => ExitCode::from(code as u8),
            Err(error) => {
                eprintln!("cabind browser: {error}");
                ExitCode::FAILURE
            }
        },
        Some("device-probe") => {
            let report = agent_core::device_probe::probe();
            match serde_json::to_writer(std::io::stdout().lock(), &report) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("cabind device-probe: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("dns-serve") => match agent_core::guest_dns::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("cabind dns-serve: {error}");
                ExitCode::FAILURE
            }
        },
        Some("guest-desktop") => guest_desktop(&args[1..]),
        Some("guest-system") => guest_system(&args[1..]),
        Some("install-service") => match install_service(&paths) {
            Ok(()) => {
                println!("cabind service installed and running");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("cabind: {error}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!(
                "usage: cabind serve | connect | install-service | native-display | sync <scan|pack|apply|watch> <root> | browser <command> | device-probe | dns-serve | guest-desktop <command> | guest-system <command> | --version"
            );
            ExitCode::from(2)
        }
    }
}
