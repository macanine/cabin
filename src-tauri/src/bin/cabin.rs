#[cfg(unix)]
fn main() -> std::process::ExitCode {
    match cabin_desktop_lib::native_cli::run(std::env::args().skip(1).collect()) {
        Ok(code) => std::process::ExitCode::from(code),
        Err(error) => {
            eprintln!("cabin: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("The experimental Cabin CLI currently requires a Unix host");
    std::process::ExitCode::FAILURE
}
