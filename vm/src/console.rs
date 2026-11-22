//! Interactive serial console client (no SSH, no extra dependencies).
//!
//! QEMU exposes the guest PL011 UART through a socket chardev.  This puts
//! the local terminal in raw mode, forwards keystrokes to the guest and prints
//! guest output.  Ctrl-] detaches without stopping the VM.
//!
//! The raw-mode terminal plumbing is POSIX-only; other hosts keep the attach
//! error path (the desktop app never attaches a console).

use std::path::Path;

use crate::Result;
#[cfg(not(unix))]
use crate::util::message;

#[cfg(unix)]
mod raw {
    use std::io::{Read, Write};
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::time::Duration;

    use crate::Result;
    use crate::util::message;

    pub fn attach(socket_path: &Path) -> Result<()> {
        if !socket_path.exists() {
            return message(format!(
                "console socket is missing: {}; start the VM with 'vmctl start --name ...'",
                socket_path.display()
            ));
        }
        let mut socket = UnixStream::connect(socket_path).map_err(|error| {
            crate::Error::Message(format!(
                "cannot connect to guest console {}: {error}",
                socket_path.display()
            ))
        })?;
        socket.set_nonblocking(true)?;
        let stdin_fd = libc::STDIN_FILENO;
        let interactive = unsafe { libc::isatty(stdin_fd) } == 1;
        let mut old_attrs: Option<libc::termios> = None;
        if interactive {
            let mut attrs: libc::termios = unsafe { std::mem::zeroed() };
            if unsafe { libc::tcgetattr(stdin_fd, &mut attrs) } == 0 {
                old_attrs = Some(attrs);
                let mut raw = attrs;
                unsafe {
                    libc::cfmakeraw(&mut raw);
                    libc::tcsetattr(stdin_fd, libc::TCSADRAIN, &raw);
                }
                eprintln!(
                    "attached to guest serial console; press Ctrl-] to detach (the VM keeps running)"
                );
            }
        }
        let mut stdout = std::io::stdout();
        let mut buffer = [0u8; 65536];
        loop {
            let mut pollfds = [
                libc::pollfd {
                    fd: socket.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: stdin_fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let ready = unsafe { libc::poll(pollfds.as_mut_ptr(), 2, -1) };
            if ready < 0 {
                break;
            }
            if pollfds[0].revents & libc::POLLIN != 0 {
                match socket.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        let _ = stdout.write_all(&buffer[..read]);
                        let _ = stdout.flush();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => break,
                }
            }
            if pollfds[1].revents & libc::POLLIN != 0 {
                match std::io::stdin().read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        if interactive && read == 1 && buffer[0] == 0x1d {
                            break;
                        }
                        if socket.write_all(&buffer[..read]).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if let Some(attrs) = old_attrs {
            unsafe {
                libc::tcsetattr(stdin_fd, libc::TCSADRAIN, &attrs);
            }
        }
        Ok(())
    }
}

pub fn attach(socket_path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        raw::attach(socket_path)
    }
    #[cfg(not(unix))]
    {
        let _ = socket_path;
        message("vmctl console requires a POSIX host")
    }
}
