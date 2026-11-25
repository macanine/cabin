// Prevents an extra console window on Windows in release; harmless on Linux.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(unix)]
    {
        let mut args = std::env::args();
        let argv0 = args.next().unwrap_or_default();
        if std::path::Path::new(&argv0)
            .file_name()
            .is_some_and(|name| name == "cabin")
        {
            std::process::exit(match cabin_desktop_lib::native_cli::run(args.collect()) {
                Ok(code) => i32::from(code),
                Err(error) => {
                    eprintln!("cabin: {error}");
                    1
                }
            });
        }
        if args.next().as_deref() == Some("--native-sync-worker") {
            let args: Vec<_> = args.collect();
            std::process::exit(match cabin_desktop_lib::native_cli::sync_worker(&args) {
                Ok(code) => i32::from(code),
                Err(error) => {
                    eprintln!("cabin: {error}");
                    1
                }
            });
        }
    }
    cabin_desktop_lib::run()
}
