//! Helpers shared by the opt-in tests that boot disposable ARM64 guests.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn build_guest_helper(dir: &Path) -> PathBuf {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let version = Command::new(&rustc).arg("-vV").output().expect("run rustc");
    assert!(version.status.success(), "rustc -vV failed");
    let version = String::from_utf8(version.stdout).unwrap();
    let host = version.lines().find_map(|line| line.strip_prefix("host: ")).expect("rustc host triple");
    let sysroot = Command::new(&rustc).args(["--print", "sysroot"]).output().expect("find Rust sysroot");
    assert!(sysroot.status.success(), "rustc --print sysroot failed");
    let sysroot = String::from_utf8(sysroot.stdout).unwrap();
    let linker = Path::new(sysroot.trim()).join("lib/rustlib").join(host).join("bin/rust-lld");
    let output = dir.join("guest-test-helper");
    let result = Command::new(&rustc)
        .args(["--edition=2024", "--target", "aarch64-unknown-linux-musl", "-C"])
        .arg(format!("linker={}", linker.display()))
        .args(["-C", "linker-flavor=ld.lld"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/guest.rs"))
        .arg("-o").arg(&output)
        .output().expect("compile guest test helper");
    assert!(result.status.success(), "compile guest test helper (install the aarch64-unknown-linux-musl Rust target): {}", String::from_utf8_lossy(&result.stderr));
    output
}

pub fn install_guest_helper(ssh: &mut Command, binary: &Path) {
    let output = ssh
        .arg("mkdir -p \"$HOME/.local/bin\" && cat > \"$HOME/.local/bin/guest-test-helper\" && chmod 755 \"$HOME/.local/bin/guest-test-helper\"")
        .stdin(Stdio::from(std::fs::File::open(binary).unwrap()))
        .output()
        .expect("install guest test helper");
    assert!(output.status.success(), "install guest test helper: {}", String::from_utf8_lossy(&output.stderr));
}
