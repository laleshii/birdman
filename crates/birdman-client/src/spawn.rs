use std::path::Path;
use std::time::{Duration, Instant};

use crate::ClientError;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);

pub fn ensure_daemon(socket: &Path) -> Result<(), ClientError> {
    if connects(socket) {
        return Ok(());
    }
    let binary = daemon_binary()?;
    log::info!("starting {}", binary.display());

    std::process::Command::new(&binary)
        // Detached: the daemon must outlive the client that started it.
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|err| {
            ClientError::Transport(format!("could not start {}: {err}", binary.display()))
        })?;

    let deadline = Instant::now() + STARTUP_TIMEOUT;
    while Instant::now() < deadline {
        if connects(socket) {
            return Ok(());
        }
        std::thread::sleep(POLL);
    }
    Err(ClientError::Transport(format!(
        "birdmand did not start within {}s -- check its log in {}",
        STARTUP_TIMEOUT.as_secs(),
        birdman_config::data_dir().display()
    )))
}

/// Shutdown is asked for, not performed. A replacement started before the old
/// daemon unlinks the socket will find it still answering and refuse to start.
pub fn wait_for_stop(socket: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !connects(socket) {
            return true;
        }
        std::thread::sleep(POLL);
    }
    false
}

/// Connects rather than stat-ing: a socket left behind by a crash still exists.
fn connects(socket: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket).is_ok()
}

/// Beside-the-binary before `PATH`, so a dev build talks to its own daemon
/// rather than a stale installed one.
fn daemon_binary() -> Result<std::path::PathBuf, ClientError> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(sibling) = exe.parent().map(|dir| dir.join("birdmand")) {
            if sibling.is_file() {
                return Ok(sibling);
            }
        }
    }
    Ok(std::path::PathBuf::from("birdmand"))
}
