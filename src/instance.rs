//! One Crate at a time: the first instance listens on a socket in
//! $XDG_RUNTIME_DIR; a second launch asks it to raise its window and exits.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
};

use tokio::sync::mpsc;

use crate::mpris::Transport;

/// Single-instance mode needs a private runtime dir (0700, per user); a
/// shared /tmp would let another user squat the socket.
fn runtime_dir() -> Option<PathBuf> {
    std::env::var("XDG_RUNTIME_DIR").ok().filter(|d| !d.is_empty()).map(PathBuf::from)
}

fn socket_path() -> Option<PathBuf> {
    runtime_dir().map(|d| d.join("crate.sock"))
}

/// Held for the primary instance's lifetime: whoever holds it owns the socket.
static LOCK: std::sync::OnceLock<std::fs::File> = std::sync::OnceLock::new();

/// Try to become the primary instance (an exclusive lock on
/// $XDG_RUNTIME_DIR/crate.lock, released by the OS when the process ends).
pub fn acquire() -> bool {
    let Some(dir) = runtime_dir() else { return true };
    let Ok(f) = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("crate.lock")) else {
        return true; // can't lock: don't block startup
    };
    if f.try_lock().is_err() {
        return false;
    }
    let _ = LOCK.set(f);
    true
}

/// Ask the running Crate to come forward (it may still be starting, so
/// retry briefly).
pub fn hand_off() -> bool {
    let Some(path) = socket_path() else { return false };
    for _ in 0..20 {
        if let Ok(mut s) = UnixStream::connect(&path) {
            return s.write_all(b"raise\n").is_ok();
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

/// As the primary instance (after `acquire`), listen for raise requests;
/// they arrive as Transport::Raise. Any socket file there is a crashed run's.
pub fn listen(tx: mpsc::UnboundedSender<Transport>) -> bool {
    let Some(path) = socket_path() else { return false };
    if LOCK.get().is_none() {
        return false;
    }
    let _ = std::fs::remove_file(&path);
    let Ok(listener) = UnixListener::bind(&path) else { return false };
    std::thread::Builder::new()
        .name("instance".into())
        .spawn(move || {
            for conn in listener.incoming().flatten() {
                // A client that never finishes its line must not block the next one.
                let _ = conn.set_read_timeout(Some(std::time::Duration::from_secs(1)));
                let mut line = String::new();
                if BufReader::new(conn).read_line(&mut line).is_ok() && line.trim() == "raise" {
                    let _ = tx.send(Transport::Raise);
                }
            }
        })
        .is_ok()
}

/// Remove our socket on the way out (only the lock holder owns it).
pub fn cleanup() {
    if LOCK.get().is_some()
        && let Some(p) = socket_path()
    {
        let _ = std::fs::remove_file(p);
    }
}

/// Bring the window forward. Wayland compositors may refuse an unprompted
/// activation, so on Hyprland ask it directly as well.
pub fn focus_hint() {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        let _ = std::process::Command::new("hyprctl")
            .args(["eval", "hl.dispatch(hl.dsp.focus({ window = 'class:^(crate)$' }))"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}
