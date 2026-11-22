#!/bin/sh
set -eu

if [ "$(uname -s)" != "Darwin" ]; then
  printf '%s\n' 'This command is for macOS. Use npm run tauri on Linux.' >&2
  exit 1
fi

# Cargo-installed tools such as cargo-zigbuild live here even when the
# invoking shell did not source the user's shell profile.
if [ -d "$HOME/.cargo/bin" ]; then
  PATH="$HOME/.cargo/bin:$PATH"
  export PATH
fi

if ! command -v cargo >/dev/null 2>&1; then
  if ! command -v rustup >/dev/null 2>&1; then
    printf '%s\n' 'Rust is required. Install rustup and a stable toolchain first.' >&2
    exit 1
  fi
  RUST_TOOLCHAIN_BIN=$(dirname "$(rustup which cargo)")
  export PATH="$RUST_TOOLCHAIN_BIN:$PATH"
fi

# The source tree may also be mounted in a Linux VM. Keep native artifacts
# separate so each platform can build without replacing the other's binaries.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/Library/Caches/cabin/target-macos}"
exec ./node_modules/.bin/tauri "$@"
