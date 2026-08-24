use birdman_client::Client;

/// The tail of the shared log, newest last.
///
/// Both this binary and the daemon append to one file, which is what makes it
/// worth a command: a sync failure happens inside `birdmand`, where nothing the
/// user typed can see it.
pub fn log(lines: usize, follow: bool) -> i32 {
    let path = birdman_config::data_dir().join("birdman.log");
    let show = |from: u64| -> u64 {
        let Ok(text) = std::fs::read_to_string(&path) else {
            return from;
        };
        let len = text.len() as u64;
        if from == 0 {
            for line in text
                .lines()
                .rev()
                .take(lines)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                println!("{line}");
            }
        } else if len > from {
            print!("{}", &text[from as usize..]);
        }
        len
    };

    if !path.exists() {
        eprintln!("no log at {}", path.display());
        return 1;
    }
    let mut seen = show(0);
    if !follow {
        return 0;
    }
    loop {
        std::thread::sleep(std::time::Duration::from_millis(400));
        seen = show(seen.max(1));
    }
}

pub fn status() -> i32 {
    let socket = Client::socket_path();
    if !Client::is_running() {
        println!("not running  ({})", socket.display());
        return 0;
    }
    match Client::connect() {
        Ok(client) => {
            println!(
                "running      {}{}",
                socket.display(),
                match read_pid() {
                    Some(pid) => format!("  (pid {pid})"),
                    None => String::new(),
                }
            );
            match client.accounts() {
                Ok(accounts) => {
                    for account in accounts {
                        println!("  {} ({})", account.display_name, account.email);
                    }
                }
                Err(err) => println!("  (could not list accounts: {err})"),
            }
            0
        }
        Err(err) => {
            println!("running      {}", socket.display());
            println!("but          {err}");
            1
        }
    }
}

pub fn stop() -> i32 {
    if !Client::is_running() {
        println!("not running");
        return 0;
    }
    // Straight to the socket: `Client::connect` would refuse on the handshake,
    // and a version-mismatched daemon is exactly the one you need to stop.
    match stop_without_handshake() {
        Ok(()) => {
            println!("stopped");
            0
        }
        Err(err) => {
            eprintln!("could not stop it: {err}");
            1
        }
    }
}

pub fn restart() -> i32 {
    if Client::is_running() && stop_without_handshake().is_err() {
        eprintln!("could not stop the running daemon");
        return 1;
    }
    // Wait for the socket to go before starting: binding races otherwise.
    for _ in 0..50 {
        if !Client::is_running() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    match Client::connect() {
        Ok(_) => {
            println!("restarted");
            0
        }
        Err(err) => {
            eprintln!("could not start it: {err}");
            1
        }
    }
}

fn stop_without_handshake() -> std::io::Result<()> {
    if ask_politely().is_ok() && wait_for_exit() {
        return Ok(());
    }
    match read_pid() {
        Some(pid) => {
            // SIGTERM, not SIGKILL: the daemon has a store to close.
            unsafe { libc::kill(pid, libc::SIGTERM) };
            if wait_for_exit() {
                let socket = Client::socket_path();
                let _ = std::fs::remove_file(&socket);
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

fn ask_politely() -> std::io::Result<()> {
    use std::io::{BufRead, BufReader, Write};
    let mut stream = std::os::unix::net::UnixStream::connect(Client::socket_path())?;
    writeln!(stream, r#"{{"id":1,"kind":"shutdown"}}"#)?;
    stream.flush()?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply)?;
    // An older daemon answers with an error rather than closing, so the reply
    // proves nothing; `wait_for_exit` decides.
    Ok(())
}

fn read_pid() -> Option<i32> {
    std::fs::read_to_string(Client::socket_path().with_extension("pid"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn wait_for_exit() -> bool {
    for _ in 0..40 {
        if !Client::is_running() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}
