// Prevents an extra console window on Windows in release; harmless on Linux.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(unix)]
    {
        let mut args = std::env::args_os();
        if args
            .next()
            .as_deref()
            .and_then(|arg| std::path::Path::new(arg).file_name())
            .is_some_and(|name| name == "cabin" || name == "desktop-cli")
        {
            std::process::exit(
                match cli_args(args).and_then(cabin_desktop_lib::native_cli::run) {
                    Ok(code) => i32::from(code),
                    Err(error) => {
                        eprintln!("cabin: {error}");
                        1
                    }
                },
            );
        }
        if args.next().is_some_and(|arg| arg == "--native-sync-worker") {
            std::process::exit(
                match cli_args(args)
                    .and_then(|args| cabin_desktop_lib::native_cli::sync_worker(&args))
                {
                    Ok(code) => i32::from(code),
                    Err(error) => {
                        eprintln!("cabin: {error}");
                        1
                    }
                },
            );
        }
    }
    cabin_desktop_lib::run()
}

#[cfg(unix)]
fn cli_args(args: std::env::ArgsOs) -> Result<Vec<String>, String> {
    args.map(|arg| {
        arg.into_string()
            .map_err(|_| "command arguments must be valid UTF-8".to_string())
    })
    .collect()
}
