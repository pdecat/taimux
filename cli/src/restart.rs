//! Putting a claude session back on the version that is installed now.
//!
//! Step 4 of replacing bash, and the part where a mistake costs a turn of work
//! rather than a redraw. Everything here is built around one rule: **a pane it
//! cannot be sure about is skipped, with a reason.** Guessing which conversation
//! a pane is on and then interrupting it is worse than doing nothing.
//!
//! The guards, in the order they are applied, and what each is for:
//!
//! 1. **Not this pane.** Restarting the pane taimux is running in would kill
//!    taimux mid-restart.
//! 2. **Only a stale version.** A pane already on the installed version has
//!    nothing to gain from being interrupted.
//! 3. **Idle**, unless `--include-busy`. A turn in flight is work in progress.
//! 4. **A conversation that could be identified**, from the ladder in `conv`.
//! 5. **No dialog on screen**, ever, even with `--include-busy`: a half-answered
//!    permission prompt is the one state where a Ctrl-C means something else.
//! 6. **No unsent draft, and a settled transcript**, unless `--include-busy`.
//! 7. **No transcript claimed twice.** Resuming one conversation into two panes
//!    is the same mistake on a restart as on a restore.

use std::collections::HashMap;

use taimux_core::{conv, state, tmux};

/// Is this transcript quiet enough to interrupt?
///
/// Two refusals: an unanswered `tool_use` as the very last record (a tool call is
/// still in flight), and any activity in the last 45 seconds. The second is
/// insurance for the first, because the screen reading is the most fragile thing
/// in this tool and a recently-touched transcript is worth leaving alone whatever
/// the screen said.
///
/// The `tool_use` test is a text match on the final line rather than a parse of
/// `.message.content[]`, which is the one approximation: an assistant turn whose
/// own prose quotes that key reads as busy. It fails toward "leave it alone", and
/// the recency guard covers the same ground, so the cost is a pane skipped rather
/// than a turn lost.
pub fn transcript_is_settled(text: &str, now: i64) -> Result<(), String> {
    let Some(last) = text.lines().rfind(|l| !l.trim().is_empty()) else {
        return Ok(()); // unknown shape: do not block on a guess
    };
    if taimux_core::json::field(last, "type") == "assistant"
        && (last.contains("\"type\":\"tool_use\"") || last.contains("\"type\": \"tool_use\""))
    {
        return Err("a tool call is still running".into());
    }
    let ts = taimux_core::json::field(last, "timestamp");
    if ts.is_empty() {
        return Ok(());
    }
    let Some(then) = parse_iso8601(&ts) else {
        return Ok(()); // unparseable: not evidence of anything
    };
    if now - then < 45 {
        return Err("active in the last 45s".into());
    }
    Ok(())
}

/// An ISO 8601 UTC timestamp to epoch seconds, which is all claude writes:
/// `2026-09-02T01:23:45.678Z`.
///
/// Hand-rolled rather than shelling to `date -d`, which is what bash did per
/// pane. Only the shape claude actually emits is accepted; anything else reads as
/// "no answer", and the caller treats that as not-evidence rather than as busy.
fn parse_iso8601(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let n = |a: usize, z: usize| s[a..z].parse::<i64>().ok();
    let (y, mo, d) = (n(0, 4)?, n(5, 7)?, n(8, 10)?);
    let (h, mi, sec) = (n(11, 13)?, n(14, 16)?, n(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + sec)
}

/// Days since 1970-01-01 for a civil date. Howard Hinnant's algorithm, which is
/// exact for every date and has no calendar table to get wrong.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Is the screen free of anything a Ctrl-C would mean something else to?
///
/// Applied even with `--include-busy`, because this is the one state where the
/// keystroke that starts a restart is also an answer to a question.
pub fn screen_has_no_dialog(screen: &str) -> Result<(), String> {
    if state::awaits_input(screen) {
        return Err("a dialog is waiting for an answer".into());
    }
    let tail: Vec<&str> = screen
        .lines()
        .filter(|l| !l.trim().is_empty())
        .rev()
        .take(4)
        .collect();
    if tail
        .iter()
        .any(|l| l.contains("Press Ctrl-C again to exit"))
    {
        return Err("a Ctrl-C is already half-pressed".into());
    }
    Ok(())
}

/// Is the prompt box empty?
///
/// Unsent text in it is work nobody has committed yet, and a restart would throw
/// it away. The non-breaking space claude pads the box with is stripped before
/// the check, or every box would look occupied.
pub fn screen_has_no_draft(screen: &str) -> Result<(), String> {
    let txt: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
    if txt.is_empty() {
        return Ok(());
    }
    let Some(box_line) = txt.iter().rev().find(|l| l.contains('❯')) else {
        return Err("no prompt box on screen".into());
    };
    let after = box_line.split_once('❯').map(|(_, r)| r).unwrap_or("");
    let rest: String = after
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{a0}')
        .collect();
    if rest.is_empty() {
        Ok(())
    } else {
        Err("unsent text in the prompt box".into())
    }
}

/// The version a running process is executing, read from its own `/proc/exe`
/// rather than from the binary on `$PATH`: a long-lived session goes on running
/// the release it started under.
pub fn version_of_pid(pid: i32, versions_dir: &str) -> Option<String> {
    let exe = std::fs::read_link(format!("/proc/{}/exe", pid)).ok()?;
    let exe = exe.to_string_lossy();
    let exe = exe.trim_end_matches(" (deleted)"); // replaced by an update
    let prefix = format!("{}/", versions_dir);
    exe.strip_prefix(&prefix)
        .filter(|rest| !rest.is_empty())
        .map(|rest| rest.split('/').next().unwrap_or(rest).to_string())
}

/// One pane the plan will act on.
pub struct Planned {
    pub pane: String,
    pub target: String,
    pub pid: i32,
    pub cmd: String,
    pub title: String,
    pub via: String,
}

pub struct Plan {
    pub newver: String,
    pub launcher: String,
    pub go: Vec<Planned>,
    pub skipped: Vec<String>,
    /// Set when at least one pane was skipped for being unresolvable, which gets
    /// its own paragraph: it is the one skip the reader can act on.
    pub unresolved: bool,
}

pub struct Opts {
    pub include_busy: bool,
    pub only_panes: Vec<String>,
    pub force_transcript: Option<String>,
    pub self_pane: String,
}

/// Everything the plan needs from the outside, so it can be built against
/// fixtures as well as against a live machine.
pub trait Env {
    fn capture(&self, pane: &str) -> String;
    fn hook_state(&self, pane: &str, pid: i32) -> Option<String>;
    fn version_of_pid(&self, pid: i32) -> Option<String>;
    fn cwd_of(&self, pid: i32) -> Option<String>;
    fn argv_of(&self, pid: i32) -> Vec<String>;
    fn resolve(&self, pane: &str, cwd: &str, title: &str, pid: i32) -> Result<String, String>;
    fn read_transcript(&self, path: &str) -> Option<String>;
    fn now(&self) -> i64;
}

/// Decide what to restart, and say why for everything else.
///
/// `agents` is `claude agents --json`, fetched by the caller and only when a
/// stale non-pane process actually needs naming: the CLI costs about three
/// seconds to start.
pub fn plan(
    rows: &str,
    newver: &str,
    launcher: &str,
    o: &Opts,
    e: &dyn Env,
    versions_dir: &str,
    agents: &dyn Fn() -> String,
) -> Plan {
    let mut p = Plan {
        newver: newver.to_string(),
        launcher: launcher.to_string(),
        go: Vec::new(),
        skipped: Vec::new(),
        unresolved: false,
    };
    let mut claimed: HashMap<String, String> = HashMap::new();
    // Every pane's agent pid, so the non-pane sweep below can tell a session
    // that has a pane from one that has not.
    let mut matched: Vec<i32> = Vec::new();

    for line in rows.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 7 || f[3] != "claude" {
            continue;
        }
        let (id, tgt, cwd, title) = (f[0], f[1], f[2], f[6]);
        let pid: i32 = f[4].parse().unwrap_or(0);

        let Some(ver) = (pid != 0).then(|| e.version_of_pid(pid)).flatten() else {
            p.skipped.push(format!(
                "{} {}  looks like claude but no session process was found under the pane",
                id, tgt
            ));
            continue;
        };
        matched.push(pid);
        if !o.only_panes.is_empty() && !o.only_panes.iter().any(|w| w == id) {
            continue;
        }
        if ver == newver {
            continue; // nothing to gain from interrupting it
        }
        if id == o.self_pane {
            p.skipped.push(format!(
                "{} {}  {}, this pane: restart it by hand (killing it would kill taimux)",
                id, tgt, ver
            ));
            continue;
        }

        let screen = e.capture(id);
        let st = state::merge(state::classify(&screen), e.hook_state(id, pid).as_deref());
        if st.as_str() != "idle" && !o.include_busy {
            p.skipped.push(format!(
                "{} {}  {}, {}: rerun when idle, or --include-busy",
                id,
                tgt,
                ver,
                st.as_str()
            ));
            continue;
        }

        let ccwd = e.cwd_of(pid).unwrap_or_else(|| cwd.to_string());
        let (transcript, via) = match &o.force_transcript {
            Some(t) => (t.clone(), "--transcript, given".to_string()),
            None => match e.resolve(id, &ccwd, title, pid) {
                Ok(t) => {
                    // resolve() hands back "<path>\t<why>"
                    let (path, why) = t.split_once('\t').unwrap_or((t.as_str(), ""));
                    (path.to_string(), why.to_string())
                }
                Err(why) => {
                    p.skipped
                        .push(format!("{} {}  {}, unresolved: {}", id, tgt, ver, why));
                    p.unresolved = true;
                    continue;
                }
            },
        };

        if let Err(why) = screen_has_no_dialog(&screen) {
            p.skipped
                .push(format!("{} {}  {}, not settled: {}", id, tgt, ver, why));
            continue;
        }
        if !o.include_busy {
            let settled =
                screen_has_no_draft(&screen).and_then(|()| match e.read_transcript(&transcript) {
                    Some(text) => transcript_is_settled(&text, e.now()),
                    None => Ok(()),
                });
            if let Err(why) = settled {
                p.skipped
                    .push(format!("{} {}  {}, not settled: {}", id, tgt, ver, why));
                continue;
            }
        }

        if let Some(first) = claimed.get(&transcript) {
            p.skipped.push(format!(
                "{} {}  {}, resolves to the same transcript as {}",
                id, tgt, ver, first
            ));
            continue;
        }
        claimed.insert(transcript.clone(), id.to_string());

        let Some(cmd) = conv::build_cmd(&e.argv_of(pid), &transcript, &ccwd, cwd) else {
            p.skipped
                .push(format!("{} {}  {}, could not read its argv", id, tgt, ver));
            continue;
        };
        p.go.push(Planned {
            pane: id.to_string(),
            target: tgt.to_string(),
            pid,
            cmd,
            title: title.to_string(),
            via: format!("{} -> {}, {}, {}", ver, newver, via, st.as_str()),
        });
    }

    // Stale claude processes that are not a pane's foreground job. Reported,
    // never touched: a restart types into a PANE, and these have none, so the
    // only useful thing to do about a stale one is name it. The naming costs a
    // `claude agents --json`, so it is fetched lazily and once.
    let mut agents_json: Option<String> = None;
    for np in nonpane_pids(&matched, versions_dir) {
        let Some(ver) = e.version_of_pid(np) else {
            continue;
        };
        if ver == newver {
            continue;
        }
        let j = agents_json.get_or_insert_with(agents);
        p.skipped.push(format!(
            "pid {}  {}, not a tmux pane: {}",
            np,
            ver,
            describe_nonpane(&e.argv_of(np), j)
        ));
    }
    p
}

/// The plan, as the reader sees it. Byte-for-byte what bash printed, because the
/// only way to know a port of this is right is to compare it.
pub fn render(p: &Plan) -> String {
    let mut s = format!("claude: {} installed at {}\n\n", p.newver, p.launcher);
    if p.go.is_empty() {
        s.push_str("nothing to restart.\n");
    } else {
        s.push_str(&format!("to restart ({}):\n", p.go.len()));
        for g in &p.go {
            s.push_str(&format!("  {:<5} {:<14} {}\n", g.pane, g.target, g.title));
            s.push_str(&format!("        {}\n", g.via));
            s.push_str(&format!("        {}\n", g.cmd));
        }
    }
    if !p.skipped.is_empty() {
        s.push_str(&format!("\nskipped ({}):\n", p.skipped.len()));
        for k in &p.skipped {
            s.push_str(&format!("  {}\n", k));
        }
    }
    if p.unresolved {
        s.push_str(
            "\nAn unresolved pane means guessing, so it was left alone: restart it by hand\n\
             with `claude -c` in that pane, or from its /resume picker. Each session records\n\
             its pane at the next SessionStart, so a pane resolves cleanly once restarted.\n",
        );
    }
    s
}

/// Interrupt a session, wait for it to go, and type its replacement.
///
/// Two Ctrl-Cs, because the first one arms claude's "press again to exit" and the
/// second takes it. `/exit` after six waits is for a session that ignores both,
/// and thirty waits (twelve seconds) is where it gives up rather than typing a
/// command into a pane that still has a session in it.
pub fn restart_pane(pane: &str, pid: i32, cmd: &str) -> bool {
    use std::thread::sleep;
    use std::time::Duration;

    tmux::run(&["send-keys", "-t", pane, "C-c"]);
    sleep(Duration::from_millis(300));
    tmux::run(&["send-keys", "-t", pane, "C-c"]);

    let mut waited = 0;
    let mut sent_exit = false;
    while alive(pid) {
        sleep(Duration::from_millis(400));
        waited += 1;
        if waited >= 6 && !sent_exit {
            tmux::run(&["send-keys", "-t", pane, "/exit", "Enter"]);
            sent_exit = true;
        }
        if waited >= 30 {
            return false;
        }
    }
    sleep(Duration::from_millis(500));
    tmux::run(&["send-keys", "-t", pane, "C-c"]);
    sleep(Duration::from_millis(200));
    tmux::run(&["send-keys", "-t", pane, cmd, "Enter"])
}

/// Is a pid still there? `/proc` rather than `kill -0`, since nothing is being
/// signalled and a directory read cannot be mistaken for one.
fn alive(pid: i32) -> bool {
    std::path::Path::new(&format!("/proc/{}", pid)).exists()
}

/// The default answer is yes, so a bare Enter restarts. The plan has already been
/// printed by then, which is what makes that safe.
pub fn confirm_yes(answer: &str) -> bool {
    matches!(answer.trim(), "" | "y" | "Y" | "yes" | "YES" | "Yes")
}

/// The live implementation of everything `plan` needs.
pub struct Live {
    pub versions_dir: String,
}

impl Env for Live {
    fn capture(&self, pane: &str) -> String {
        tmux::ask_raw(&["capture-pane", "-p", "-t", pane]).unwrap_or_default()
    }
    fn hook_state(&self, pane: &str, pid: i32) -> Option<String> {
        taimux_core::hook::hook_state_of(pane, pid)
    }
    fn version_of_pid(&self, pid: i32) -> Option<String> {
        version_of_pid(pid, &self.versions_dir)
    }
    fn cwd_of(&self, pid: i32) -> Option<String> {
        std::fs::read_link(format!("/proc/{}/cwd", pid))
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
    }
    fn argv_of(&self, pid: i32) -> Vec<String> {
        conv::argv_of(pid)
    }
    fn resolve(&self, pane: &str, cwd: &str, title: &str, pid: i32) -> Result<String, String> {
        conv::resolve(pane, cwd, title, pid)
            .map(|r| format!("{}\t{}", r.transcript.display(), r.why))
    }
    fn read_transcript(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }
    fn now(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }
}

/// What a session started right now would run, refusing rather than guessing when
/// the launcher points somewhere unexpected.
pub fn installed(launcher: &str, versions_dir: &str) -> Result<String, String> {
    let target = std::fs::canonicalize(launcher)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let prefix = format!("{}/", versions_dir);
    match target.strip_prefix(&prefix) {
        Some(rest) if !rest.is_empty() => Ok(rest.split('/').next().unwrap_or(rest).to_string()),
        _ => Err(format!(
            "restart: {} does not point into {} (got '{}')",
            launcher,
            versions_dir,
            if target.is_empty() {
                "nothing"
            } else {
                &target
            }
        )),
    }
}

pub fn versions_dir() -> String {
    format!(
        "{}/.local/share/claude/versions",
        std::env::var("HOME").unwrap_or_default()
    )
}

pub fn launcher() -> String {
    format!(
        "{}/.local/bin/claude",
        std::env::var("HOME").unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timestamp_becomes_epoch_seconds() {
        // 2026-09-02T01:23:45Z
        assert_eq!(parse_iso8601("2026-09-02T01:23:45.678Z"), Some(1788312225));
        assert_eq!(parse_iso8601("1970-01-01T00:00:00Z"), Some(0));
        // a leap day, which a hand-rolled calendar is where it goes wrong
        assert_eq!(parse_iso8601("2024-02-29T00:00:00Z"), Some(1709164800));
    }

    /// Anything not of that exact shape reads as "no answer", which the caller
    /// treats as not-evidence rather than as busy: guessing busy would skip a
    /// pane on a malformed line forever.
    #[test]
    fn an_unparseable_timestamp_is_no_answer() {
        assert_eq!(parse_iso8601(""), None);
        assert_eq!(parse_iso8601("yesterday"), None);
        assert_eq!(parse_iso8601("2026-09-02 01:23:45"), None);
        assert_eq!(parse_iso8601("2026-13-02T01:23:45Z"), None);
    }

    #[test]
    fn a_tool_call_still_running_is_not_settled() {
        let t = r#"{"type":"assistant","message":{"content":[{"type":"tool_use"}]},"timestamp":"2020-01-01T00:00:00Z"}"#;
        assert_eq!(
            transcript_is_settled(t, 1788312225),
            Err("a tool call is still running".into())
        );
    }

    /// Insurance for the screen reading, which is the most fragile thing here.
    #[test]
    fn recent_activity_is_not_settled_whatever_the_screen_said() {
        let t = r#"{"type":"user","timestamp":"2026-09-02T01:23:45Z"}"#;
        assert!(transcript_is_settled(t, 1788312225 + 10).is_err());
        assert!(transcript_is_settled(t, 1788312225 + 100).is_ok());
    }

    #[test]
    fn an_empty_or_odd_transcript_does_not_block() {
        assert!(transcript_is_settled("", 0).is_ok());
        assert!(transcript_is_settled("not json at all\n", 0).is_ok());
        // no timestamp: nothing to judge recency by
        assert!(transcript_is_settled(r#"{"type":"user"}"#, 0).is_ok());
    }

    /// A dialog is what `state::awaits_input` recognises, so the fixture has to
    /// be one it would: either the footer in the last few lines, or a numbered
    /// choice on the LOWEST prompt line. A made-up shape asserts nothing.
    #[test]
    fn a_dialog_on_screen_blocks_a_restart() {
        let footer = "some output\n\nDo you want to proceed?\n";
        assert_eq!(
            screen_has_no_dialog(footer),
            Err("a dialog is waiting for an answer".into())
        );
        let choice = "some output\n1. Yes\n2. No\n❯ 1. Yes\n";
        assert_eq!(
            screen_has_no_dialog(choice),
            Err("a dialog is waiting for an answer".into())
        );
        // an ordinary idle screen is not a dialog
        assert!(screen_has_no_dialog("some output\n❯ \n").is_ok());
    }

    /// A half-pressed Ctrl-C is the one state where the keystroke that starts a
    /// restart is also an answer to something else.
    #[test]
    fn a_half_pressed_ctrl_c_blocks_a_restart() {
        let screen = "work\n\nPress Ctrl-C again to exit\n";
        assert_eq!(
            screen_has_no_dialog(screen),
            Err("a Ctrl-C is already half-pressed".into())
        );
    }

    #[test]
    fn an_empty_prompt_box_is_no_draft() {
        assert!(screen_has_no_draft("stuff\n❯ \n").is_ok());
        // the non-breaking space claude pads the box with is not a draft
        assert!(screen_has_no_draft("stuff\n❯ \u{a0}\u{a0}\n").is_ok());
        assert_eq!(
            screen_has_no_draft("stuff\n❯ half a thought\n"),
            Err("unsent text in the prompt box".into())
        );
    }

    /// No box at all means the screen is not what it is expected to be, and that
    /// is a refusal rather than a shrug.
    #[test]
    fn no_prompt_box_is_a_refusal() {
        assert_eq!(
            screen_has_no_draft("just some text\n"),
            Err("no prompt box on screen".into())
        );
        // …but a genuinely blank screen is not judged at all
        assert!(screen_has_no_draft("").is_ok());
        assert!(screen_has_no_draft("   \n\n").is_ok());
    }

    #[test]
    fn a_bare_enter_confirms() {
        assert!(confirm_yes(""));
        assert!(confirm_yes("y"));
        assert!(confirm_yes("YES"));
        assert!(!confirm_yes("n"));
        assert!(!confirm_yes("no"));
        assert!(!confirm_yes("maybe"));
    }

    #[test]
    fn the_launcher_must_point_into_the_versions_dir() {
        let root = std::env::temp_dir().join(format!("jmrs{}", std::process::id()));
        let vers = root.join("versions");
        std::fs::create_dir_all(&vers).unwrap();
        std::fs::write(vers.join("2.1.258"), "x").unwrap();
        let link = root.join("claude");
        std::os::unix::fs::symlink(vers.join("2.1.258"), &link).unwrap();
        assert_eq!(
            installed(&link.to_string_lossy(), &vers.to_string_lossy()),
            Ok("2.1.258".into())
        );
        // pointing elsewhere is a refusal that names what it found
        std::fs::write(root.join("elsewhere"), "x").unwrap();
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(root.join("elsewhere"), &link).unwrap();
        let e = installed(&link.to_string_lossy(), &vers.to_string_lossy()).expect_err("refused");
        assert!(e.contains("elsewhere"));
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// A claude process that is not any pane's foreground job: Zed's ACP bridge, a
/// background agent, one of the daemon's spare pty hosts.
///
/// Reported, never touched. A restart types into a PANE, and these have none, so
/// the only useful thing to do about a stale one is name it.
pub fn nonpane_pids(matched: &[i32], versions_dir: &str) -> Vec<i32> {
    let mut out = Vec::new();
    for e in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else {
            continue;
        };
        if matched.contains(&pid) {
            continue;
        }
        // Either its comm is "claude" (pgrep -x claude) or it is running out of
        // the versions directory (pgrep -f "^<vdir>/").
        let comm = std::fs::read_to_string(format!("/proc/{}/comm", pid)).unwrap_or_default();
        let argv = conv::argv_of(pid);
        let is_claude = comm.trim() == "claude"
            || argv
                .first()
                .map(|a| a.starts_with(&format!("{}/", versions_dir)))
                .unwrap_or(false);
        if is_claude {
            out.push(pid);
        }
    }
    out.sort_unstable();
    out
}

/// The object in a JSON array whose `key` starts with `prefix`, as raw text.
///
/// A brace matcher rather than a parser, and rather than the `jq` the bash
/// version shelled out to. It only has to find one element of one array, and the
/// depth count is what makes it safe against nested objects: taking everything
/// between the first `{` and the first `}` would truncate any element with a
/// nested field.
fn json_object_with_prefix(text: &str, key: &str, prefix: &str) -> Option<String> {
    let needle = format!("\"{}\":\"{}", key, prefix);
    let at = text.find(&needle).or_else(|| {
        let spaced = format!("\"{}\": \"{}", key, prefix);
        text.find(&spaced)
    })?;
    // back to the opening brace of the object holding it
    let mut depth = 0i32;
    let bytes = text.as_bytes();
    let mut start = None;
    for i in (0..at).rev() {
        match bytes[i] {
            b'}' => depth += 1,
            b'{' => {
                if depth == 0 {
                    start = Some(i);
                    break;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    let start = start?;
    // forward to its matching close
    let mut depth = 0i32;
    for i in start..bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// What to say about a claude that has no pane.
///
/// `agents_json` is `claude agents --json`, which costs about three seconds of
/// CLI startup, so it is fetched once by the caller and only when something needs
/// naming. Its own `pid` field points at the pty-host wrapper rather than at the
/// session, so it is read for kind, state and name and nothing else.
pub fn describe_nonpane(argv: &[String], agents_json: &str) -> String {
    if argv.is_empty() {
        return "gone".into();
    }
    let (mut sid, mut res, mut forked) = (String::new(), String::new(), false);
    for i in 1..argv.len() {
        match argv[i].as_str() {
            "--fork-session" => forked = true,
            "--session-id" => sid = argv.get(i + 1).cloned().unwrap_or_default(),
            "-r" | "--resume" => res = argv.get(i + 1).cloned().unwrap_or_default(),
            _ => {}
        }
    }
    let own = base_id(if sid.is_empty() { &res } else { &sid });
    let parent = if forked && !res.is_empty() {
        base_id(&res)
    } else {
        String::new()
    };

    let (mut kind, mut st, mut name) = ("?".to_string(), "?".to_string(), String::new());
    if !own.is_empty() {
        if let Some(obj) = json_object_with_prefix(agents_json, "sessionId", &own) {
            let f = |k: &str| taimux_core::json::field(&obj, k);
            let k = f("kind");
            if !k.is_empty() {
                kind = k;
            }
            let s = {
                let a = f("state");
                if a.is_empty() {
                    f("status")
                } else {
                    a
                }
            };
            if !s.is_empty() {
                st = s;
            }
            name = f("name");
        }
    }
    let mut out = format!("{} {}", kind, st);
    if forked {
        out.push_str(" fork");
    }
    if !name.is_empty() {
        out.push_str(&format!(" \"{}\"", name));
    }
    if !own.is_empty() {
        out.push_str(&format!(" [{}]", own.chars().take(8).collect::<String>()));
    }
    if !parent.is_empty() {
        out.push_str(&format!(
            " of [{}]",
            parent.chars().take(8).collect::<String>()
        ));
    }
    out
}

/// `basename x .jsonl`: a session id, whether it arrived as one or as a path.
fn base_id(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    let b = s.rsplit('/').next().unwrap_or(s);
    b.strip_suffix(".jsonl").unwrap_or(b).to_string()
}

#[cfg(test)]
mod nonpane_tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_session_id_is_read_out_of_a_path_or_taken_as_it_stands() {
        assert_eq!(base_id("/a/b/263946b5-9bd7.jsonl"), "263946b5-9bd7");
        assert_eq!(base_id("263946b5"), "263946b5");
        assert_eq!(base_id(""), "");
    }

    /// The depth count is the whole point: taking everything between the first
    /// brace and the first close would truncate any element with a nested field,
    /// and `claude agents --json` has them.
    #[test]
    fn the_right_object_comes_back_whole() {
        let j = r#"[{"sessionId":"aaa111","kind":"task","meta":{"a":1},"name":"first"},
                    {"sessionId":"bbb222","kind":"agent","name":"second"}]"#;
        let o = json_object_with_prefix(j, "sessionId", "bbb222").expect("found");
        assert!(o.contains("second"));
        assert!(!o.contains("first"));
        let o = json_object_with_prefix(j, "sessionId", "aaa").expect("found by prefix");
        assert!(o.contains("first"));
        assert!(
            o.contains("\"meta\":{\"a\":1}"),
            "nested field truncated: {}",
            o
        );
        assert!(json_object_with_prefix(j, "sessionId", "zzz").is_none());
    }

    #[test]
    fn a_process_with_no_argv_is_simply_gone() {
        assert_eq!(describe_nonpane(&[], "[]"), "gone");
    }

    #[test]
    fn an_unnamed_session_still_says_what_it_can() {
        let d = describe_nonpane(&v(&["claude", "--session-id", "263946b5-9bd7"]), "[]");
        assert_eq!(d, "? ? [263946b5]");
    }

    #[test]
    fn a_named_one_says_kind_state_and_name() {
        let j =
            r#"[{"sessionId":"263946b5-9bd7","kind":"task","state":"running","name":"the thing"}]"#;
        let d = describe_nonpane(&v(&["claude", "--session-id", "263946b5-9bd7"]), j);
        assert_eq!(d, "task running \"the thing\" [263946b5]");
    }

    /// `status` is the older field name, and one of the two is what a given
    /// version emits.
    #[test]
    fn status_stands_in_for_state() {
        let j = r#"[{"sessionId":"aaa11111","kind":"agent","status":"idle"}]"#;
        let d = describe_nonpane(&v(&["claude", "--session-id", "aaa11111"]), j);
        assert_eq!(d, "agent idle [aaa11111]");
    }

    /// A fork names both itself and its parent, because that is the pair you need
    /// to work out which one is the stale one.
    #[test]
    fn a_fork_names_its_parent_too() {
        let d = describe_nonpane(
            &v(&[
                "claude",
                "--session-id",
                "child111",
                "--fork-session",
                "--resume",
                "/p/parent22.jsonl",
            ]),
            "[]",
        );
        assert_eq!(d, "? ? fork [child111] of [parent22]");
    }
}

#[cfg(test)]
mod plan_tests {
    use super::*;

    /// A machine with one stale pane, one current one, and whatever else the test
    /// asks for. The `Env` trait exists for this: the live differential can only
    /// reach "nothing to restart" while every session is on the installed
    /// version, so the branch that actually acts needs a fixture.
    struct Fake {
        /// pid -> version
        vers: HashMap<i32, String>,
        /// pane -> screen
        screens: HashMap<String, String>,
        /// pane -> resolved transcript, or the refusal
        resolved: HashMap<String, Result<String, String>>,
        transcript: String,
        now: i64,
    }

    impl Env for Fake {
        fn capture(&self, pane: &str) -> String {
            self.screens.get(pane).cloned().unwrap_or_default()
        }
        fn hook_state(&self, _pane: &str, _pid: i32) -> Option<String> {
            None
        }
        fn version_of_pid(&self, pid: i32) -> Option<String> {
            self.vers.get(&pid).cloned()
        }
        fn cwd_of(&self, _pid: i32) -> Option<String> {
            Some("/w".into())
        }
        fn argv_of(&self, _pid: i32) -> Vec<String> {
            vec!["claude".into()]
        }
        fn resolve(&self, pane: &str, _c: &str, _t: &str, _p: i32) -> Result<String, String> {
            self.resolved
                .get(pane)
                .cloned()
                .unwrap_or_else(|| Err("no candidate".into()))
        }
        fn read_transcript(&self, _path: &str) -> Option<String> {
            Some(self.transcript.clone())
        }
        fn now(&self) -> i64 {
            self.now
        }
    }

    fn fake() -> Fake {
        let mut vers = HashMap::new();
        vers.insert(11, "2.1.100".to_string()); // stale
        vers.insert(22, "2.1.258".to_string()); // current
        let mut screens = HashMap::new();
        // an idle screen with an empty prompt box
        screens.insert("%1".to_string(), "some output\n❯ \n".to_string());
        screens.insert("%2".to_string(), "some output\n❯ \n".to_string());
        let mut resolved = HashMap::new();
        resolved.insert("%1".to_string(), Ok("/t/a.jsonl\tpane map".to_string()));
        Fake {
            vers,
            screens,
            resolved,
            transcript: r#"{"type":"user","timestamp":"2020-01-01T00:00:00Z"}"#.to_string(),
            now: 1788312225,
        }
    }

    fn opts() -> Opts {
        Opts {
            include_busy: false,
            only_panes: Vec::new(),
            force_transcript: None,
            self_pane: String::new(),
        }
    }

    const ROWS: &str = "%1\tw:1.1\t/w\tclaude\t11\tclaude\tproj: the stale one\n\
                        %2\tw:2.1\t/w\tclaude\t22\tclaude\tproj: the current one";

    fn plan_of(e: &Fake, o: &Opts) -> Plan {
        plan(ROWS, "2.1.258", "/l/claude", o, e, "/nowhere", &|| {
            "[]".into()
        })
    }

    #[test]
    fn a_stale_pane_is_planned_and_a_current_one_is_not() {
        let p = plan_of(&fake(), &opts());
        assert_eq!(p.go.len(), 1);
        assert_eq!(p.go[0].pane, "%1");
        assert_eq!(p.go[0].cmd, "command claude --resume /t/a.jsonl");
        assert_eq!(p.go[0].via, "2.1.100 -> 2.1.258, pane map, idle");
        assert!(p.skipped.is_empty());
    }

    /// The rendered plan is the whole user interface of `restart -n`, so its
    /// exact shape is what the bash comparison was made on.
    #[test]
    fn the_rendered_plan_reads_the_way_it_always_did() {
        let out = render(&plan_of(&fake(), &opts()));
        assert_eq!(
            out,
            "claude: 2.1.258 installed at /l/claude\n\n\
             to restart (1):\n\
             \x20 %1    w:1.1          proj: the stale one\n\
             \x20       2.1.100 -> 2.1.258, pane map, idle\n\
             \x20       command claude --resume /t/a.jsonl\n"
        );
    }

    /// Restarting the pane taimux is running in would kill taimux mid-restart.
    #[test]
    fn this_pane_is_never_restarted() {
        let mut o = opts();
        o.self_pane = "%1".into();
        let p = plan_of(&fake(), &o);
        assert!(p.go.is_empty());
        assert!(p.skipped[0].contains("this pane"));
        assert!(p.skipped[0].contains("killing it would kill taimux"));
    }

    #[test]
    fn a_pane_that_is_not_idle_waits_for_include_busy() {
        let mut e = fake();
        // an activity line: mid-turn
        e.screens
            .insert("%1".into(), "Twisting… (35s · ↓ 1.6k tokens)\n❯ \n".into());
        let p = plan_of(&e, &opts());
        assert!(p.go.is_empty());
        assert!(p.skipped[0].contains("rerun when idle, or --include-busy"));

        let mut o = opts();
        o.include_busy = true;
        assert_eq!(plan_of(&e, &o).go.len(), 1);
    }

    /// Even with --include-busy: a half-answered permission prompt is the one
    /// state where the Ctrl-C that starts a restart means something else.
    #[test]
    fn a_dialog_blocks_a_restart_even_with_include_busy() {
        let mut e = fake();
        e.screens
            .insert("%1".into(), "output\n\nDo you want to proceed?\n".into());
        let mut o = opts();
        o.include_busy = true;
        let p = plan_of(&e, &o);
        assert!(p.go.is_empty());
        assert!(p.skipped[0].contains("a dialog is waiting for an answer"));
    }

    #[test]
    fn an_unsent_draft_is_left_alone() {
        let mut e = fake();
        e.screens
            .insert("%1".into(), "output\n❯ half a thought\n".into());
        let p = plan_of(&e, &opts());
        assert!(p.skipped[0].contains("unsent text in the prompt box"));
    }

    /// A transcript touched in the last 45 seconds is left alone whatever the
    /// screen said, because the screen reading is the fragile half.
    #[test]
    fn a_recently_active_transcript_is_left_alone() {
        let mut e = fake();
        e.transcript = r#"{"type":"user","timestamp":"2026-09-02T01:23:45Z"}"#.into();
        e.now = 1788312225 + 10;
        let p = plan_of(&e, &opts());
        assert!(p.skipped[0].contains("active in the last 45s"));
    }

    #[test]
    fn an_unresolved_pane_says_so_and_earns_the_paragraph() {
        let mut e = fake();
        e.resolved
            .insert("%1".into(), Err("3 transcripts share this title".into()));
        let p = plan_of(&e, &opts());
        assert!(p.go.is_empty());
        assert!(p.unresolved);
        assert!(p.skipped[0].contains("unresolved: 3 transcripts share this title"));
        assert!(render(&p).contains("restart it by hand"));
    }

    /// Two panes on one conversation is the same mistake on a restart as on a
    /// restore: the second one to claim it gets skipped rather than resumed.
    #[test]
    fn one_transcript_is_never_resumed_into_two_panes() {
        let mut e = fake();
        e.vers.insert(22, "2.1.100".into()); // make the second one stale too
        e.resolved
            .insert("%2".into(), Ok("/t/a.jsonl\tpane map".into()));
        let p = plan_of(&e, &opts());
        assert_eq!(p.go.len(), 1);
        assert_eq!(p.go[0].pane, "%1");
        assert!(p.skipped[0].contains("resolves to the same transcript as %1"));
    }

    #[test]
    fn only_panes_narrows_the_plan_without_changing_the_verdicts() {
        let mut o = opts();
        o.only_panes = vec!["%2".into()];
        let p = plan_of(&fake(), &o);
        assert!(p.go.is_empty());
        assert!(p.skipped.is_empty()); // %1 was not considered at all
    }

    #[test]
    fn a_pane_with_no_session_process_is_reported_not_dropped() {
        let rows = "%9\tw:9.9\t/w\tclaude\t0\tclaude\tno pid here";
        let p = plan(rows, "2.1.258", "/l", &opts(), &fake(), "/nowhere", &|| {
            "[]".into()
        });
        assert!(p.go.is_empty());
        assert_eq!(p.skipped.len(), 1);
        assert!(p.skipped[0].contains("no session process was found"));
    }

    #[test]
    fn nothing_to_restart_says_so() {
        let mut e = fake();
        e.vers.insert(11, "2.1.258".into());
        let out = render(&plan_of(&e, &opts()));
        assert!(out.contains("nothing to restart."));
        assert!(!out.contains("to restart ("));
    }

    /// --transcript names the conversation outright, which is the escape hatch
    /// for a pane the ladder refuses.
    #[test]
    fn a_given_transcript_overrides_the_ladder() {
        let mut e = fake();
        e.resolved.insert("%1".into(), Err("no candidate".into()));
        let mut o = opts();
        o.force_transcript = Some("/given.jsonl".into());
        let p = plan_of(&e, &o);
        assert_eq!(p.go.len(), 1);
        assert!(p.go[0].via.contains("--transcript, given"));
        assert!(p.go[0].cmd.contains("--resume /given.jsonl"));
    }
}
