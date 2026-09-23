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
use std::path::Path;
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::{Duration, Instant};

use taimux_core::{panes, paths, version};

/// What this build is, for the handshake below. Every crate in the workspace
/// carries the same literal version and release-please keeps them in step, so
/// the daemon crate's own version IS the binary's.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The shape of the rows this build serves, which the `rows` handshake names
/// beside the version.
///
/// The version alone cannot say it. A build from the checkout changes the rows
/// without a release to bump the version, so a daemon the build before started
/// would pass the check and go on serving the old shape: rows still well-formed,
/// only missing what the new picker reads, with nothing to show it but a sort
/// that quietly stopped doing anything. Bump it whenever a row gains, loses or
/// moves a field.
const SHAPE: &str = "rows/2";

const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// How often the loop looks up from waiting to ask whether it has been idle long
/// enough to exit. Nothing waits on this: a request wakes the loop immediately.
const TICK: Duration = Duration::from_secs(5);
/// How long a captured screen may be reused. Short on purpose: the state it is
/// read for is the whole point of the list, and a stale one is worse than a slow
/// one.
const CAPTURE_TTL: Duration = Duration::from_millis(750);
/// The protocol, such as it is: one request word per line, a body, then a blank
/// line. Newline-delimited and human-readable on purpose, so a client can be a
/// shell, a socat, or eventually the picker itself, and so a wedged daemon can be
/// diagnosed by hand.
///
/// Answers `false` when the request was to stop, which is the only one that says
/// anything about the daemon rather than about the panes.
fn handle(
    stream: UnixStream,
    prober: &mut version::Prober,
    captures: &mut HashMap<String, String>,
) -> bool {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return true,
    });
    let mut out = stream;
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return true;
    }
    let body = match line.trim() {
        "panes" => panes::agent_rows(),
        "list" => panes::list_rows(prober, captures),
        // The same rows, behind one line saying which build produced them.
        //
        // A daemon outlives the binary that started it: a self-update or a
        // `just build` replaces the file, every later client is the new code,
        // and the process still listening is the old. Its rows would then be
        // built by whatever the scan used to do, which is invisible precisely
        // because they are still well-formed rows. So the client compares this
        // line against its own version and does the work itself when they
        // differ, and a daemon too old to know this word answers `!unknown
        // request`, which is the same answer to the same question.
        //
        // One request rather than a `version` round trip before every `list`,
        // because the two have to describe the SAME answer: a daemon replaced
        // between the two calls would pass the check and then serve rows from
        // the other build.
        "rows" => format!(
            "{} {}\n{}",
            VERSION,
            SHAPE,
            panes::list_rows(prober, captures)
        ),
        "version" => format!("{}\n", VERSION),
        "ping" => "pong\n".to_string(),
        "quit" => "bye\n".to_string(),
        other => format!("!unknown request: {}\n", other),
    };
    let _ = out.write_all(body.as_bytes());
    let _ = out.write_all(b"\n");
    let _ = out.flush();
    line.trim() != "quit"
}

pub fn serve() -> std::io::Result<()> {
    let path = paths::socket_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Bind FIRST, and only ask questions about the path when that is refused.
    //
    // A socket file outlives the process that made it, so a daemon that was
    // killed leaves one behind that nothing is listening on, and it has to be
    // cleared or nothing can ever bind again. This used to probe, unlink and
    // then bind, which is the wrong order once daemons START THEMSELVES: two
    // pickers opening together both found nobody listening, both unlinked, and
    // both bound, leaving the first one holding a socket no client could reach
    // and waiting out its idle timeout for nothing.
    //
    // Unlinking only after `AddrInUse` AND a connection that nothing answers
    // means the file being removed is proven stale rather than assumed to be.
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if UnixStream::connect(&path).is_ok() {
                eprintln!("taimux: already running at {}", path.display());
                return Ok(());
            }
            let _ = std::fs::remove_file(&path);
            UnixListener::bind(&path)?
        }
        Err(e) => return Err(e),
    };

    let mut last = Instant::now();
    let mut prober = version::Prober::new();
    // Screens go stale fast, so the cache is only good for one burst: it exists
    // to stop three clients in the same refresh tick each paying 26 captures,
    // not to serve an old picture of a session.
    let mut captures: HashMap<String, String> = HashMap::new();
    let mut captured_at = Instant::now();

    // Accept BLOCKS, on a thread of its own, and the loop below waits on the
    // channel. It polled instead until 2026-09-20: a non-blocking accept and a
    // 50 ms sleep, which put up to 50 ms of latency in front of every request
    // for a daemon that was otherwise doing nothing. Measured on 35 panes, that
    // was the difference between the socket path costing 64 ms and costing
    // 128 ms, against 80 ms for a client that never asked at all: the caching
    // this daemon exists for was being spent on the wait to be heard.
    //
    // The obvious repair, a shorter sleep, trades that for a wakeup every few
    // milliseconds all day. Blocking costs neither: the kernel wakes the thread
    // when a client connects, and the timeout below is only how often the loop
    // looks up to ask whether it has been idle long enough to go home.
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            // A client that has gone, or a main loop that has, both end it: the
            // first cannot be answered and the second is on its way out.
            match stream {
                Ok(s) => {
                    if tx.send(s).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    loop {
        match rx.recv_timeout(TICK) {
            Ok(stream) => {
                if captured_at.elapsed() > CAPTURE_TTL {
                    captures.clear();
                    captured_at = Instant::now();
                }
                if !handle(stream, &mut prober, &mut captures) {
                    break; // asked to stand down, see `quit`
                }
                last = Instant::now();
            }
            Err(RecvTimeoutError::Timeout) => {
                if last.elapsed() > IDLE_TIMEOUT {
                    break;
                }
            }
            // The accept thread has gone, so nothing can arrive any more.
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = std::fs::remove_file(&path);
    Ok(())
}

/// Ask a running daemon, for a caller that wants the answer rather than to print
/// it. `None` means there is nobody to ask, which every client treats as "do the
/// work yourself" rather than as an error.
pub fn ask(request: &str) -> Option<String> {
    ask_at(&paths::socket_path(), request)
}

fn ask_at(path: &Path, request: &str) -> Option<String> {
    let mut stream = UnixStream::connect(path).ok()?;
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

/// The picker's rows from a daemon that is running THIS build, or `None`.
///
/// `None` covers every way asking can fail to be worth trusting, and they are
/// all the same answer to the caller, "do it yourself": nobody listening, which
/// is the normal state on a host that has never started one; a daemon that
/// hangs, which the read timeout in `ask_at` turns into a refusal; one too old
/// to know the request; and one running a different build, which is the case
/// that has no symptom of its own, since rows from the previous version are
/// still perfectly well-formed rows.
pub fn rows() -> Option<String> {
    rows_at(&paths::socket_path())
}

fn rows_at(path: &Path) -> Option<String> {
    let body = ask_at(path, "rows")?;
    let (ver, rows) = body.split_once('\n')?;
    if ver.split_once(' ') != Some((VERSION, SHAPE)) {
        // …and ask it to stand down, or nothing ever replaces it. A daemon goes
        // home after five idle minutes, but being ASKED is what keeps it from
        // being idle, and a picker refusing this one is still asking it every
        // refresh: left alone, the old build would be kept alive for as long as
        // anyone used the picker, and the new one could never take the socket.
        //
        // Fire and forget. A daemon older than this word answers `!unknown
        // request` and stays, which is no worse than before it was sent, and
        // the client stops asking that socket for rows either way.
        let _ = ask_at(path, "quit");
        return None;
    }
    // The protocol frames a body with a trailing blank line, so what arrives is
    // one newline longer than the rows are. Normalised here rather than left to
    // the caller, because the whole claim of this answer is that it is the same
    // bytes the caller would have scanned for itself, and it was not: `taimux
    // list` through a daemon has always printed one blank line at the end,
    // which the suite could not see because it tests that path with nothing
    // listening. Harmless where a row parser skips short lines, and the sort of
    // difference that is only ever found by the next thing to read it.
    let rows = rows.trim_end_matches('\n');
    Some(if rows.is_empty() {
        String::new()
    } else {
        format!("{}\n", rows)
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A daemon that answers one request with whatever it was given, so the
    /// client half can be driven without a real one. Its own socket, so nothing
    /// here touches `TAIMUX_SOCKET` or the developer's live daemon.
    /// A `rows` answer from a daemon running this very build.
    fn this_build(rows: &str) -> &'static str {
        Box::leak(format!("{} {}\n{}", VERSION, SHAPE, rows).into_boxed_str())
    }

    fn fake(reply: &'static str) -> (std::path::PathBuf, std::thread::JoinHandle<String>) {
        let (path, handle) = serve_replies(vec![reply]);
        (
            path,
            std::thread::spawn(move || handle.join().unwrap().join(",")),
        )
    }

    /// Two requests on one socket, for the exchange a refusal turns into.
    fn fake_twice(
        first: &'static str,
        second: &'static str,
    ) -> (std::path::PathBuf, std::thread::JoinHandle<Vec<String>>) {
        serve_replies(vec![first, second])
    }

    fn serve_replies(
        replies: Vec<&'static str>,
    ) -> (std::path::PathBuf, std::thread::JoinHandle<Vec<String>>) {
        let path = std::env::temp_dir().join(format!(
            "taimux-test-{}-{:?}.sock",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind the test socket");
        let handle = std::thread::spawn(move || {
            let mut asked = Vec::new();
            for reply in replies {
                let (stream, _) = listener.accept().expect("a client");
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                let _ = reader.read_line(&mut line);
                let mut out = stream;
                let _ = out.write_all(reply.as_bytes());
                asked.push(line.trim().to_string());
            }
            asked
        });
        (path, handle)
    }

    #[test]
    fn rows_come_back_from_a_daemon_on_this_version() {
        let (path, srv) = fake(this_build("%1\tw:1.1\t/h\n"));
        assert_eq!(rows_at(&path).as_deref(), Some("%1\tw:1.1\t/h\n"));
        assert_eq!(srv.join().unwrap(), "rows", "one request, not a handshake");
        let _ = std::fs::remove_file(&path);
    }

    /// …and with the blank line the protocol really frames a body with, which is
    /// what a live daemon sends. Without the trim that reaches the caller as a
    /// row of no fields, and `taimux list` prints it: true of the socket path
    /// since it existed, and invisible to a suite that tests it with nothing
    /// listening.
    #[test]
    fn the_frames_trailing_blank_line_is_not_part_of_the_rows() {
        let (path, srv) = fake(this_build("%1\tw:1.1\t/h\n\n"));
        assert_eq!(rows_at(&path).as_deref(), Some("%1\tw:1.1\t/h\n"));
        let _ = srv.join();
        let _ = std::fs::remove_file(&path);
    }

    /// The case with no symptom of its own: a daemon left running by the build
    /// before this one answers rows that LOOK right, and the picker would show
    /// them. Every other refusal is visible; this one has to be checked for.
    #[test]
    fn a_daemon_on_another_version_is_refused() {
        let (path, srv) = fake("0.0.1\n%1\tw:1.1\t/h\n");
        assert_eq!(rows_at(&path), None);
        let _ = srv.join();
        let _ = std::fs::remove_file(&path);
    }

    /// …and is told to stand down, because refusing it is not enough: asking is
    /// what keeps a daemon from being idle, so a picker that merely ignored the
    /// old build would keep it alive for as long as it ran and the new one could
    /// never have the socket.
    #[test]
    fn a_refused_daemon_is_asked_to_stand_down() {
        let (path, srv) = fake_twice("0.0.1\n%1\tw:1.1\t/h\n", "bye\n");
        assert_eq!(rows_at(&path), None);
        assert_eq!(srv.join().unwrap(), vec!["rows", "quit"]);
        let _ = std::fs::remove_file(&path);
    }

    /// A daemon on this very version still serving the old row shape, which is
    /// what a build from the checkout leaves running when it changes the rows
    /// without a release: refused and stood down like any other build, or the
    /// new picker would sort on a field its rows do not have.
    #[test]
    fn a_daemon_serving_the_old_row_shape_is_refused() {
        let (path, srv) = fake_twice(
            concat!(env!("CARGO_PKG_VERSION"), "\n%1\tw:1.1\t/h\n"),
            "bye\n",
        );
        assert_eq!(rows_at(&path), None);
        assert_eq!(srv.join().unwrap(), vec!["rows", "quit"]);
        let _ = std::fs::remove_file(&path);
    }

    /// A daemon too old to know the word answers the error the protocol already
    /// had, which `ask_at` turns into the same "do it yourself".
    #[test]
    fn a_daemon_too_old_for_the_request_is_refused() {
        let (path, srv) = fake("!unknown request: rows\n");
        assert_eq!(rows_at(&path), None);
        let _ = srv.join();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn nobody_listening_is_not_an_error() {
        assert_eq!(rows_at(Path::new("/nonexistent/taimux.sock")), None);
        assert_eq!(ask_at(Path::new("/nonexistent/taimux.sock"), "rows"), None);
    }

    /// An empty answer is a daemon saying there are no agent panes, which is not
    /// the same as no daemon: it must come back as `Some("")`, or the picker
    /// would scan again behind it every refresh on an idle machine.
    #[test]
    fn a_daemon_with_no_rows_still_answers() {
        let (path, srv) = fake(this_build("\n"));
        assert_eq!(rows_at(&path).as_deref(), Some(""));
        let _ = srv.join();
        let _ = std::fs::remove_file(&path);
    }
}
