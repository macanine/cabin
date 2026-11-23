fn main() {
    if std::env::args().nth(1).as_deref() == Some("--vmctl-proxy-relay") {
        std::process::exit(vm_core::net_relay::run());
    }
    // Match normal Unix CLI behaviour when a downstream pipe closes (`| head`).
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let argv: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(vm_core::cli::main(&argv));
}
