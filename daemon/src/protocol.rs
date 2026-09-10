//! The daemon, and the protocol both ends of it speak.
//!
//! What the daemon is FOR is memoisation, not work: it holds one version prober
//! and one short-lived screen cache across connections, so a picker refreshing
//! every few seconds does not re-fork `date`, re-read every agent binary and
//! re-capture every pane each time. Everything it can answer, a client can also
//! work out for itself, which is why every caller falls back rather than failing
//! when there is nobody listening.
//!
//! Both halves live here because they are one contract. The server and the two
//! client helpers were in three different places in main.rs, and the wire format
//! was a match arm in the middle of a dispatcher.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::{Duration, Instant};

use taimux_core::{panes, paths, version};

const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// How long a captured screen may be reused. Short on purpose: the state it is
/// read for is the whole point of the list, and a stale one is worse than a slow
/// one.
const CAPTURE_TTL: Duration = Duration::from_millis(750);
/// The protocol, such as it is: one request word per line, a body, then a blank
/// line. Newline-delimited and human-readable on purpose, so a client can be a
/// shell, a socat, or eventually the picker itself, and so a wedged daemon can be
/// diagnosed by hand.
fn handle(
    stream: UnixStream,
    prober: &mut version::Prober,
    captures: &mut HashMap<String, String>,
) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut out = stream;
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let body = match line.trim() {
        "panes" => panes::agent_rows(),
        "list" => panes::list_rows(prober, captures),
        "ping" => "pong\n".to_string(),
        other => format!("!unknown request: {}\n", other),
    };
    let _ = out.write_all(body.as_bytes());
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

pub fn serve() -> std::io::Result<()> {
    let path = paths::socket_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // A socket file outlives the process that made it, so a daemon that was
    // killed leaves one behind that nothing is listening on. Clearing it is safe
    // precisely because a live one would refuse the bind below.
    if UnixStream::connect(&path).is_ok() {
        eprintln!("taimux: already running at {}", path.display());
        return Ok(());
    }
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;

    let mut last = Instant::now();
    let mut prober = version::Prober::new();
    // Screens go stale fast, so the cache is only good for one burst: it exists
    // to stop three clients in the same refresh tick each paying 26 captures,
    // not to serve an old picture of a session.
    let mut captures: HashMap<String, String> = HashMap::new();
    let mut captured_at = Instant::now();
    listener.set_nonblocking(true)?;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if captured_at.elapsed() > CAPTURE_TTL {
                    captures.clear();
                    captured_at = Instant::now();
                }
                let _ = stream.set_nonblocking(false);
                handle(stream, &mut prober, &mut captures);
                last = Instant::now();
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if last.elapsed() > IDLE_TIMEOUT {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => break,
        }
    }
    let _ = std::fs::remove_file(&path);
    Ok(())
}

/// Ask a running daemon, for a caller that wants the answer rather than to print
/// it. `None` means there is nobody to ask, which every client treats as "do the
/// work yourself" rather than as an error.
pub fn ask(request: &str) -> Option<String> {
    let mut stream = UnixStream::connect(paths::socket_path()).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    stream.write_all(request.as_bytes()).ok()?;
    stream.write_all(b"\n").ok()?;
    stream.flush().ok()?;
    let mut body = String::new();
    stream.read_to_string(&mut body).ok()?;
    if body.starts_with('!') {
        return None;
    }
    Some(body)
}

/// Ask a running daemon, printing its answer. Exits non-zero when there is
/// nobody to ask, which is the signal the caller falls back on.
pub fn query(request: &str) -> std::io::Result<()> {
    let mut stream = UnixStream::connect(paths::socket_path())?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(request.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut body = String::new();
    stream.read_to_string(&mut body)?;
    if body.starts_with('!') {
        eprint!("{}", body);
        std::process::exit(2);
    }
    print!("{}", body.trim_end_matches('\n'));
    if !body.trim_end_matches('\n').is_empty() {
        println!();
    }
    Ok(())
}
