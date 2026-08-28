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

/// Stops a daemon without the protocol handshake.
///
/// [`crate::Connection::open`] refuses on a version mismatch, and a
/// version-mismatched daemon is exactly the one that has to be stopped, so this
/// speaks the wire format directly rather than going through `Client`.
pub fn stop_without_handshake(socket: &Path) -> std::io::Result<()> {
    if ask_politely(socket).is_ok() && wait_for_exit(socket) {
        return Ok(());
    }
    match read_pid(socket) {
        Some(pid) => {
            // SIGTERM, not SIGKILL: the daemon has a store to close.
            unsafe { libc::kill(pid, libc::SIGTERM) };
            if wait_for_exit(socket) {
                let _ = std::fs::remove_file(socket);
                let _ = std::fs::remove_file(socket.with_extension("pid"));
                Ok(())
            } else {
                Err(std::io::Error::other("it did not exit"))
            }
        }
        None => Err(std::io::Error::other(
            "it did not answer a shutdown request and left no pid file \
             (a daemon from an older build) -- `pkill -f birdmand`",
        )),
    }
}

fn ask_politely(socket: &Path) -> std::io::Result<()> {
    use std::io::{BufRead, BufReader, Write};
    let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
    writeln!(stream, r#"{{"id":1,"kind":"shutdown"}}"#)?;
    stream.flush()?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply)?;
    // An older daemon answers with an error rather than closing, so the reply
    // proves nothing; `wait_for_exit` decides.
    Ok(())
}

fn read_pid(socket: &Path) -> Option<i32> {
    std::fs::read_to_string(socket.with_extension("pid"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn wait_for_exit(socket: &Path) -> bool {
    wait_for_stop(socket, Duration::from_secs(2))
}
