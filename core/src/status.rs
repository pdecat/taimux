//! What Claude Code says it is doing, in its own words.
//!
//! Every interactive Claude Code process keeps `<config>/sessions/<pid>.json`
//! about itself and rewrites it whenever its state changes: `status` is `busy`,
//! `waiting` or `idle`, and a waiting one says what for in `waitingFor`
//! (`permission prompt`, `input needed`, `dialog open`). That is the session's
//! own state machine rather than a reconstruction of it, so a row asks it first
//! and falls back to the hook line and the screen only where it says nothing.
//!
//! Measured on 2.1.288 against both of those, in a throwaway tmux server, each
//! time from the keypress:
//!
//! - a permission **granted** turned it `busy` in 46 ms, where no hook fires
//!   until the tool has finished (eight seconds later in the probe);
//! - an **Esc** turned it `idle` in about 100 ms, mid-reply and on a dialog
//!   alike, where no hook fires at all;
//! - a dialog turned it `waiting` within 80 ms, where `PermissionRequest` also
//!   fires in auto mode and the notification confirming one comes 6 s late.
//!
//! It does not carry the permission mode, nor whether a finished turn left work
//! in flight, so the hook line still answers both of those.
//!
//! The file is not a documented interface. `claude agents --json` is, and reads
//! the same store, so a file whose shape is not recognised here is asked about
//! there instead (`from_cli`). A file that is not there at all is a version that
//! does not write one, or a process that is not a session, and gets no answer.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::json;
use crate::state::State;

/// One session, as Claude Code describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Own {
    /// `busy`, `waiting` or `idle`, as Claude Code spells them: see `state`.
    pub status: String,
    /// What a waiting session waits on, or empty.
    pub waiting_for: String,
    /// The conversation the process is on now. It follows a `/clear` at once,
    /// which the pane map and the argv do not.
    pub session_id: String,
    /// Where the session was started, which is what names its project folder.
    pub cwd: String,
    /// When the status last changed, in epoch milliseconds, or 0.
    pub at: i64,
    /// Where the answer came from.
    pub via: Via,
}

/// Where an answer came from: `taimux state` says, and only a process's own
/// file may name the conversation that process is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// `sessions/<pid>.json` of the process itself.
    OwnFile,
    /// The background session a `claude attach <id>` pane shows.
    Attached,
    /// The background session this process's conversation was moved into, by a
    /// backgrounding that left the process behind as its client.
    MovedTo,
    /// `claude agents --json`, for a file whose shape was not recognised.
    Cli,
}

impl Via {
    pub fn describe(self) -> &'static str {
        match self {
            Via::OwnFile => "its own status file",
            Via::Attached => "the background session it attaches to",
            Via::MovedTo => "the background session its conversation moved to",
            Via::Cli => "claude agents --json",
        }
    }
}

impl Own {
    /// The state a row shows. None for a status this does not know, which the
    /// caller then reads the old way rather than guess at.
    pub fn state(&self) -> Option<State> {
        match self.status.as_str() {
            "waiting" => Some(State::Input),
            "busy" => Some(State::Run),
            "idle" => Some(State::Idle),
            _ => None,
        }
    }

    /// The transcript this conversation is written to, if it is where Claude
    /// Code puts it. A session that has not been sent a prompt yet has none.
    pub fn transcript(&self) -> Option<PathBuf> {
        if self.session_id.is_empty() || self.session_id.contains('/') || self.cwd.is_empty() {
            return None;
        }
        let p = crate::conv::project_dir_for(&self.cwd).join(format!("{}.jsonl", self.session_id));
        p.is_file().then_some(p)
    }
}

/// The fields read out of one session file, or one `claude agents --json` entry.
#[derive(Debug, Default, PartialEq, Eq)]
struct Fields {
    pid: Option<i64>,
    proc_start: Option<String>,
    status: Option<String>,
    waiting_for: Option<String>,
    session_id: Option<String>,
    cwd: Option<String>,
    at: Option<i64>,
}

/// Top-level fields only: a session file also lists, nested, the conversations
/// it was on before (`formerNames`), each with a `sessionId` of its own.
fn fields(obj: &str) -> Fields {
    let t = json::top_level(obj);
    Fields {
        pid: json::number(&t, "pid"),
        proc_start: json::scan(&t, "procStart"),
        status: json::scan(&t, "status"),
        waiting_for: json::scan(&t, "waitingFor"),
        session_id: json::scan(&t, "sessionId"),
        cwd: json::scan(&t, "cwd"),
        at: json::number(&t, "statusUpdatedAt"),
    }
}

/// What a file says about the process holding `pid` now.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Own(Own),
    /// Written by an earlier process that had the same pid.
    Stale,
    /// No status where one was expected: the format moved on.
    Unfamiliar,
}

/// `started` is the process's start time as `/proc` gives it now. A file whose
/// `procStart` disagrees was left by a process that died, the pid having been
/// handed out again since.
fn judge(f: Fields, pid: i32, started: Option<&str>, via: Via) -> Verdict {
    if f.pid.is_some_and(|p| p != i64::from(pid)) {
        return Verdict::Stale;
    }
    if let (Some(theirs), Some(now)) = (f.proc_start.as_deref(), started) {
        if theirs != now {
            return Verdict::Stale;
        }
    }
    match f.status {
        Some(status) => Verdict::Own(Own {
            status,
            waiting_for: f.waiting_for.unwrap_or_default(),
            session_id: f.session_id.unwrap_or_default(),
            cwd: f.cwd.unwrap_or_default(),
            at: f.at.unwrap_or(0),
            via,
        }),
        None => Verdict::Unfamiliar,
    }
}

fn started(pid: i32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
    crate::proc::start_time(&stat).map(str::to_string)
}

/// Where Claude Code keeps one file per running session.
pub fn sessions_dir() -> PathBuf {
    crate::conv::claude_dir().join("sessions")
}

/// What the claude process `pid` says it is doing, or None.
///
/// A pane running `claude attach <id>` holds no session of its own: it shows a
/// background one, which runs in agent view's supervisor under another pid, and
/// that session's file is the one read.
///
/// `TAIMUX_CLAUDE_STATUS=0` turns the whole reading off, which leaves every row
/// to the hook line and the screen exactly as before it existed.
pub fn of(pid: i32) -> Option<Own> {
    if pid <= 0 || !crate::env::on("TAIMUX_CLAUDE_STATUS") {
        return None;
    }
    read_in(
        &sessions_dir(),
        pid,
        &|| crate::conv::argv_of(pid),
        &from_cli,
    )
}

/// `of`, with the store, the argv and the fallback handed in. The argv is only
/// read for a pid with no file, which is the one case it can answer.
fn read_in(
    dir: &Path,
    pid: i32,
    argv: &dyn Fn() -> Vec<String>,
    cli: &dyn Fn(i32) -> Option<Own>,
) -> Option<Own> {
    match std::fs::read_to_string(dir.join(format!("{}.json", pid))) {
        Ok(text) => match judge(fields(&text), pid, started(pid).as_deref(), Via::OwnFile) {
            Verdict::Own(o) => match moved_to(&o) {
                Some(next) => attached_to(&next, dir, Via::MovedTo),
                None => Some(o),
            },
            Verdict::Stale => None,
            Verdict::Unfamiliar => cli(pid),
        },
        Err(_) => attached_to(&crate::conv::attach_id(&argv())?, dir, Via::Attached),
    }
}

/// Where the conversation went, when it is no longer this process's to report.
///
/// Backgrounding a session mid-turn (Claude Code 2.1.286 at least) moves the
/// conversation into a worker under agent view's supervisor and leaves the
/// pane's process behind as its client. That process stops writing its own
/// file at that instant, so the file goes on saying whatever it said then:
/// found on a pane reading `busy` for 41 hours while it sat at an idle prompt.
/// The old transcript ends with a `continued-in` record naming the successor,
/// and that one line is all this reads.
fn moved_to(o: &Own) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(o.transcript()?).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(1024))).ok()?;
    let mut tail = Vec::new();
    f.read_to_end(&mut tail).ok()?;
    let tail = String::from_utf8_lossy(&tail);
    let last = tail.lines().rev().find(|l| !l.trim().is_empty())?;
    if json::first(last, "type").as_deref() != Some("continued-in") {
        return None;
    }
    json::scan(last, "continuedInSessionId").filter(|id| !id.is_empty())
}

/// The one live session whose id starts with `id`. Two matches name neither,
/// as `conv::transcript_by_id` refuses them.
fn attached_to(id: &str, dir: &Path, via: Via) -> Option<Own> {
    let mut hit = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(pid) = name
            .strip_suffix(".json")
            .and_then(|s| s.parse::<i32>().ok())
        else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(e.path()) else {
            continue;
        };
        let f = fields(&text);
        if !f.session_id.as_deref().is_some_and(|s| s.starts_with(id)) {
            continue;
        }
        // its process has to be there to vouch for the file
        let Some(now) = started(pid) else { continue };
        if let Verdict::Own(o) = judge(f, pid, Some(&now), via) {
            if hit.is_some() {
                return None;
            }
            hit = Some(o);
        }
    }
    hit
}

/// How long one `claude agents --json` answers for. Only a file in a shape this
/// does not know sends anything here, and then every such pane in a pass shares
/// one run of the CLI rather than paying for one each.
const CLI_TTL: Duration = Duration::from_secs(10);

/// The documented way to ask, for a file this cannot read.
fn from_cli(pid: i32) -> Option<Own> {
    static CACHE: Mutex<Option<(Instant, Option<String>)>> = Mutex::new(None);
    let text = {
        let mut cache = CACHE.lock().ok()?;
        match cache.as_ref() {
            Some((at, text)) if at.elapsed() < CLI_TTL => text.clone(),
            _ => {
                let text = agents_json();
                *cache = Some((Instant::now(), text.clone()));
                text
            }
        }
    }?;
    in_cli_output(&text, pid)
}

fn in_cli_output(text: &str, pid: i32) -> Option<Own> {
    json::objects(text)
        .into_iter()
        .map(fields)
        .find(|f| f.pid == Some(i64::from(pid)))
        .and_then(|f| match judge(f, pid, None, Via::Cli) {
            Verdict::Own(o) => Some(o),
            _ => None,
        })
}

/// `claude agents --json`, bounded: it answers in about a tenth of a second,
/// and a picker refresh must not wait on one that hangs.
fn agents_json() -> Option<String> {
    let home = crate::env::var("HOME").unwrap_or_default();
    // `~/.local/bin` is not on the PATH of every shell a popup starts
    for exe in ["claude".to_string(), format!("{}/.local/bin/claude", home)] {
        let Ok(mut child) = Command::new(&exe)
            .args(["agents", "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        let mut out = child.stdout.take()?;
        let reader = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = std::io::Read::read_to_string(&mut out, &mut s);
            s
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        let ok = loop {
            match child.try_wait() {
                Ok(Some(st)) => break st.success(),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break false;
                }
            }
        };
        let text = reader.join().ok()?;
        return ok.then_some(text);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file as 2.1.288 writes it, trimmed of the fields nothing reads.
    fn file(pid: i32, start: &str, status: &str, waiting: Option<&str>) -> String {
        let w = waiting.map_or(String::new(), |w| format!(",\"waitingFor\":\"{}\"", w));
        format!(
            "{{\"pid\":{pid},\"sessionId\":\"8a011e50-0f0d-437c-86e0-3a7b7abca2d3\",\"cwd\":\"/w\",\
             \"procStart\":\"{start}\",\"version\":\"2.1.288\",\"kind\":\"interactive\",\
             \"tmux\":\"probe:@0.%0\",\"status\":\"{status}\",\"statusUpdatedAt\":1791016871277{w},\
             \"formerNames\":[{{\"name\":\"old\",\"until\":1,\"sessionId\":\"c98b7d93-0000\"}}]}}"
        )
    }

    fn own(o: Verdict) -> Own {
        match o {
            Verdict::Own(o) => o,
            v => panic!("expected an answer, got {v:?}"),
        }
    }

    #[test]
    fn each_status_maps_to_the_state_a_row_shows() {
        for (status, want) in [
            ("waiting", Some(State::Input)),
            ("busy", Some(State::Run)),
            ("idle", Some(State::Idle)),
            ("compacting", None),
        ] {
            let o = own(judge(
                fields(&file(7, "99", status, None)),
                7,
                Some("99"),
                Via::OwnFile,
            ));
            assert_eq!(o.state(), want, "{status}");
        }
    }

    #[test]
    fn a_waiting_session_says_what_for_and_which_conversation_it_is_on() {
        let o = own(judge(
            fields(&file(7, "99", "waiting", Some("permission prompt"))),
            7,
            Some("99"),
            Via::OwnFile,
        ));
        assert_eq!(o.waiting_for, "permission prompt");
        assert_eq!(o.at, 1791016871277);
        // the conversation it is on now, not the one formerNames remembers
        assert_eq!(o.session_id, "8a011e50-0f0d-437c-86e0-3a7b7abca2d3");
        assert_eq!(o.cwd, "/w");
    }

    /// A pid is handed out again once its process is gone, and a file left by
    /// a claude that crashed must not speak for whatever runs under it now.
    #[test]
    fn a_file_from_an_earlier_process_with_the_same_pid_says_nothing() {
        let f = || fields(&file(7, "99", "busy", None));
        assert_eq!(judge(f(), 7, Some("12345"), Via::OwnFile), Verdict::Stale);
        assert_eq!(judge(f(), 8, Some("99"), Via::OwnFile), Verdict::Stale);
        // where /proc cannot say, the file is taken at its word
        assert!(matches!(judge(f(), 7, None, Via::OwnFile), Verdict::Own(_)));
    }

    #[test]
    fn a_file_with_no_status_is_a_format_this_does_not_know() {
        let f = fields(r#"{"pid":7,"sessionId":"s","procStart":"99","state":"busy"}"#);
        assert_eq!(judge(f, 7, Some("99"), Via::OwnFile), Verdict::Unfamiliar);
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("taimux-status-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn me() -> (i32, String) {
        let pid = std::process::id() as i32;
        (pid, started(pid).expect("this process has a start time"))
    }

    fn no_cli(_: i32) -> Option<Own> {
        panic!("the CLI was asked when the file answered")
    }

    /// Against a real process, this one, since the start-time check reads /proc.
    #[test]
    fn the_file_named_after_a_live_process_is_read() {
        // `read_in` looks for a transcript under CLAUDE_CONFIG_DIR, which another
        // test points elsewhere
        let _guard = crate::env::ENV_LOCK.lock().unwrap();
        let d = tmp("own");
        let (pid, start) = me();
        std::fs::write(
            d.join(format!("{pid}.json")),
            file(pid, &start, "busy", None),
        )
        .unwrap();
        let o = read_in(&d, pid, &Vec::new, &no_cli).expect("an answer");
        assert_eq!((o.status.as_str(), o.via), ("busy", Via::OwnFile));

        std::fs::write(d.join(format!("{pid}.json")), file(pid, "1", "busy", None)).unwrap();
        assert_eq!(read_in(&d, pid, &Vec::new, &no_cli), None, "a stale file");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_unfamiliar_file_is_asked_about_through_the_cli_and_a_missing_one_is_not() {
        // `read_in` looks for a transcript under CLAUDE_CONFIG_DIR, which another
        // test points elsewhere
        let _guard = crate::env::ENV_LOCK.lock().unwrap();
        let d = tmp("cli");
        let (pid, start) = me();
        let asked = std::cell::Cell::new(0);
        let cli = |_: i32| {
            asked.set(asked.get() + 1);
            None
        };
        assert_eq!(read_in(&d, pid, &Vec::new, &cli), None);
        assert_eq!(asked.get(), 0, "no file: a version that writes none");
        std::fs::write(
            d.join(format!("{pid}.json")),
            format!(r#"{{"pid":{pid},"procStart":"{start}","phase":"thinking"}}"#),
        )
        .unwrap();
        assert_eq!(read_in(&d, pid, &Vec::new, &cli), None);
        assert_eq!(asked.get(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// `claude attach <id>` holds no session of its own; it shows the background
    /// one whose id it names, and that session's file is under another pid.
    #[test]
    fn an_attach_pane_reads_the_background_session_it_names() {
        // `read_in` looks for a transcript under CLAUDE_CONFIG_DIR, which another
        // test points elsewhere
        let _guard = crate::env::ENV_LOCK.lock().unwrap();
        let d = tmp("attach");
        let (pid, start) = me();
        let bg = file(pid, &start, "waiting", Some("permission prompt")).replace(
            "8a011e50-0f0d-437c-86e0-3a7b7abca2d3",
            "631622b9-bfc7-4714-b1d0-0f593cafbc75",
        );
        std::fs::write(d.join(format!("{pid}.json")), bg).unwrap();
        let argv = |a: &'static [&'static str]| move || a.iter().map(|s| s.to_string()).collect();
        let attach = argv(&["claude", "attach", "631622b9"]);
        // the attach client is some other pid, with no file of its own
        let o = read_in(&d, 9_999_999, &attach, &no_cli).expect("the attached session");
        assert_eq!((o.status.as_str(), o.via), ("waiting", Via::Attached));
        // an id naming nothing, and a pane that attaches to nothing
        assert_eq!(
            read_in(
                &d,
                9_999_999,
                &argv(&["claude", "attach", "ffffffff"]),
                &no_cli
            ),
            None
        );
        assert_eq!(read_in(&d, 9_999_999, &argv(&["claude"]), &no_cli), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The pane that motivated `moved_to`: its own file froze at `busy` when its
    /// conversation was backgrounded, and the session it moved into is idle.
    #[test]
    fn a_conversation_moved_into_the_background_is_read_where_it_went() {
        let _guard = crate::env::ENV_LOCK.lock().unwrap();
        let root = tmp("moved");
        std::env::set_var("CLAUDE_CONFIG_DIR", &root);
        let dir = root.join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        let (pid, start) = me();
        // the husk: this process, its file frozen at busy
        std::fs::write(
            dir.join(format!("{pid}.json")),
            file(pid, &start, "busy", None),
        )
        .unwrap();
        let pdir = crate::conv::project_dir_for("/w");
        std::fs::create_dir_all(&pdir).unwrap();
        let t = pdir.join("8a011e50-0f0d-437c-86e0-3a7b7abca2d3.jsonl");
        std::fs::write(&t, "{\"type\":\"user\"}\n").unwrap();
        // not moved yet: its own word stands
        assert_eq!(
            read_in(&dir, pid, &Vec::new, &no_cli).unwrap().via,
            Via::OwnFile
        );

        std::fs::write(
            &t,
            "{\"type\":\"user\"}\n{\"type\":\"continued-in\",\"timestamp\":\"2026-10-01T15:52:17.718Z\",\
             \"sessionId\":\"8a011e50-0f0d-437c-86e0-3a7b7abca2d3\",\"continuedInSessionId\":\"2447b564-7a96\"}\n",
        )
        .unwrap();
        // moved, and the worker it moved into has stopped: no answer, so the
        // row falls back to the hook line and the screen
        assert_eq!(read_in(&dir, pid, &Vec::new, &no_cli), None);

        // the worker, alive
        let mut worker = Command::new("sleep").arg("30").spawn().unwrap();
        let wpid = worker.id() as i32;
        let wstart = started(wpid).unwrap();
        let bg = file(wpid, &wstart, "idle", None).replace(
            "8a011e50-0f0d-437c-86e0-3a7b7abca2d3",
            "2447b564-7a96-4774-b70a-ef4b730bdd4d",
        );
        std::fs::write(dir.join(format!("{wpid}.json")), bg).unwrap();
        let o = read_in(&dir, pid, &Vec::new, &no_cli).expect("where it went");
        assert_eq!((o.status.as_str(), o.via), ("idle", Via::MovedTo));

        let _ = worker.kill();
        let _ = worker.wait();
        std::env::remove_var("CLAUDE_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_cli_answer_is_found_by_pid() {
        let out = r#"[
  {
    "id": "631622b9",
    "kind": "background",
    "pid": 3237871,
    "sessionId": "631622b9-bfc7-4714-b1d0-0f593cafbc75",
    "status": "waiting",
    "waitingFor": "permission prompt",
    "state": "blocked"
  },
  {
    "pid": 67279,
    "cwd": "/w",
    "kind": "interactive",
    "sessionId": "8a011e50-0f0d-437c-86e0-3a7b7abca2d3",
    "name": "a } in a name",
    "status": "idle"
  }
]"#;
        let o = in_cli_output(out, 67279).expect("found");
        assert_eq!((o.status.as_str(), o.via), ("idle", Via::Cli));
        assert_eq!(
            in_cli_output(out, 3237871).unwrap().waiting_for,
            "permission prompt"
        );
        assert_eq!(in_cli_output(out, 1), None);
    }
}
