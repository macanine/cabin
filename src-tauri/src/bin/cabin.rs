#[cfg(unix)]
fn main() -> std::process::ExitCode {
    let args = std::env::args_os()
        .skip(1)
        .map(std::ffi::OsString::into_string)
        .collect::<Result<Vec<_>, _>>();
    let Ok(args) = args else {
        eprintln!("cabin: command arguments must be valid UTF-8");
        return std::process::ExitCode::FAILURE;
    };
    match cabin_desktop_lib::native_cli::run(args) {
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
