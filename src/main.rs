//! The dispatcher: every subcommand taimux answers to.
//!
//! One binary holds the lot: the picker, the pane scan, the screen reading, the
//! transcript index, federation, restart, resurrect and the installer. Nothing
//! at runtime goes through a shell.
//!
//! `serve` is still here and still worth having, because the expensive thing is
//! reading 26 pane screens and a running daemon has them cached. But it is an
//! optimisation, not a dependency: every command computes its own answer when no
//! socket answers, which is the normal case at the far end of an ssh. Getting
//! that backwards broke federation silently once (see `list`).
//!
//! Each subsystem was ported, differentially compared against the bash on live
//! data, and only then deleted. Where deleting would have taken the only
//! definition of a behaviour with it, bash's own output was frozen first:
//! tests/golden/rows.expected is 504 cases of it and cannot be regenerated.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// Rewrite a save file, or say what a rewrite would do.
///
/// Saves run detached with stdout and stderr discarded, so the log beside the
/// save file is the only place this wiring is visible between reboots.
fn resurrect_main(file: &str, dry: bool, verbose: bool) -> i32 {
    let path = std::path::Path::new(file);
    if !path.is_file() {
        return 0;
    }
    if taimux_core::tmux::ask(&["has-session"]).is_none() {
        return 0; // no server, nothing to resolve against
    }
    let log = path
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("taimux-resurrect.log");
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let note = |line: &str| {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
        {
            let _ = writeln!(f, "{} {}  {}", taimux_core::log::stamp(), name, line);
        }
    };

    // The save file names a pane by session/window/pane index, print-cmds by pane
    // id, and only tmux joins the two. Both are read while the panes are still
    // live, so the join is of one moment.
    let Some(panes) = taimux_core::tmux::ask_raw(&[
        "list-panes",
        "-a",
        "-F",
        "#{pane_id}\t#{session_name}\t#{window_index}\t#{pane_index}",
    ]) else {
        return 1;
    };
    let state_of = |id: &str, pid: i32| -> String {
        let screen = taimux_core::tmux::capture(id).unwrap_or_default();
        taimux_core::state::merge(
            &screen,
            taimux_core::hook::hook_entry(id, pid)
                .as_ref()
                .map(|(st, _)| st.as_str()),
        )
        .as_str()
        .to_string()
    };
    let records = taimux_core::conv::print_cmds(&taimux_core::panes::agent_rows(), &state_of);
    let Ok(save) = std::fs::read_to_string(path) else {
        return 1;
    };

    let o = taimux_cli::resurrect::decide(&panes, &records, &save);
    if o.total() == 0 {
        if verbose {
            println!("no claude panes");
        }
        note("no claude panes");
        return 0;
    }

    if dry {
        // A DIFF rather than the whole rewritten file, which is what makes a dry
        // run readable: only field 11 ever changes, so the diff is the change.
        // One fork to `diff` on a debugging path, exactly as bash did it.
        if !o.map.is_empty() {
            let body = taimux_cli::resurrect::rewrite(&o.map, &save);
            let tmp = std::env::temp_dir().join(format!("taimux-rw.{}", std::process::id()));
            if std::fs::write(&tmp, &body).is_ok() {
                let _ = Command::new("diff")
                    .args([
                        "-u",
                        "--label",
                        file,
                        "--label",
                        &format!("{} (rewritten)", file),
                        file,
                        &tmp.to_string_lossy(),
                    ])
                    .status();
                let _ = std::fs::remove_file(&tmp);
            }
        }
        println!("taimux resurrect: {} (dry run)", o.summary());
        for n in &o.notes {
            println!("  {}", n);
        }
        return 0;
    }

    if !o.map.is_empty() {
        // Follow the symlink rather than replacing it: `last` points at the
        // timestamped save, and resurrect reads it through that link.
        let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let original = std::fs::read_to_string(&target).unwrap_or_default();
        let body = taimux_cli::resurrect::rewrite(&o.map, &original);
        if let Err(why) = taimux_cli::resurrect::commit(&target, &body, &original) {
            note(&why);
            return 1;
        }
    }

    note(&o.summary());
    for n in &o.notes {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
        {
            let _ = writeln!(f, "    {}", n);
        }
    }
    taimux_core::log::trim_log(&log, 600, 400);
    if verbose {
        println!("taimux resurrect: {}", o.summary());
    }
    0
}

/// `taimux handoff`: the picker's ctrl-o without the picker.
///
/// With no `--to` it prints the prompt, which is the only other useful answer
/// and the one a pipeline wants. With `--to <agent>` it opens a window running
/// that agent on it, and `--print` alongside shows the command line instead of
/// running it.
fn handoff_main(a: &[String]) -> i32 {
    let flag = |n: &str| -> Option<String> {
        a.iter()
            .position(|x| x == n)
            .and_then(|i| a.get(i + 1))
            .cloned()
    };
    let Some(id) = a.first().filter(|x| !x.starts_with('-')) else {
        eprintln!("taimux handoff: which conversation? (see `taimux dead-rows`)");
        return 1;
    };
    let (agent, key) = match taimux_cli::act::conversation_of(id) {
        Ok(v) => v,
        Err(why) => {
            eprintln!("taimux handoff: {}", why);
            return 1;
        }
    };
    let turns = flag("--turns")
        .and_then(|v| v.parse().ok())
        .or_else(|| taimux_core::env::var("TAIMUX_HANDOFF_TURNS").and_then(|v| v.parse().ok()))
        .unwrap_or(taimux_core::handoff::DEFAULT_TURNS);
    let prompt = taimux_core::handoff::build(&agent, &key, turns);

    let Some(to) = flag("--to") else {
        print!("{}", prompt);
        return 0;
    };
    let cmd = match taimux_core::handoff::launch(&to, &prompt) {
        Ok(c) => c,
        Err(why) => {
            eprintln!("taimux handoff: {}", why);
            return 1;
        }
    };
    if a.iter().any(|x| x == "--print") {
        println!("{}", cmd);
        return 0;
    }
    // The same refusal Enter makes, and for the same reason: an agent started in
    // the wrong directory works on a different project, silently.
    let cwd = taimux_core::agents::meta(&agent, &key).cwd;
    if !std::path::Path::new(&cwd).is_dir() {
        eprintln!(
            "taimux handoff: {} is gone, so there is nowhere to start {}",
            if cwd.is_empty() {
                "its directory"
            } else {
                &cwd
            },
            to
        );
        return 1;
    }
    if taimux_core::tmux::run(&["new-window", "-c", &cwd, &cmd]) {
        0
    } else {
        eprintln!("taimux handoff: tmux would not open a window");
        1
    }
}

/// Build the plan, print it, and carry it out if asked.
///
/// The confirmation is only offered where there is a terminal to ask on: from
/// cron, a hook or a pipeline nobody is there to answer and blocking would hang
/// the caller, so those stay a dry run.
fn restart_main(o: &taimux_cli::restart::Opts, go: bool, ask: bool) -> i32 {
    let vdir = taimux_cli::restart::versions_dir();
    let launcher = taimux_cli::restart::launcher();
    let newver = match taimux_cli::restart::installed(&launcher, &vdir) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    };
    if let Some(t) = &o.force_transcript {
        if o.only_panes.len() != 1 {
            eprintln!("restart: --transcript needs exactly one --pane");
            return 1;
        }
        if !std::path::Path::new(t).is_file() {
            eprintln!("restart: no such transcript: {}", t);
            return 1;
        }
    }
    let env = taimux_cli::restart::Live {
        versions_dir: vdir.clone(),
    };
    // Bounded, and only asked for when a stale non-pane process needs naming:
    // the CLI takes about three seconds to start.
    let agents = || -> String {
        Command::new("claude")
            .args(["agents", "--json"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_else(|| "[]".to_string())
    };
    let plan = taimux_cli::restart::plan(
        &taimux_core::panes::agent_rows(),
        &newver,
        &launcher,
        o,
        &env,
        &vdir,
        &agents,
    );
    print!("{}", taimux_cli::restart::render(&plan));
    if plan.go.is_empty() {
        return 0;
    }
    if !go {
        if !ask {
            println!("\ndry run. pass -y to restart.");
            return 0;
        }
        // A terminal to ask on, or it stays a dry run.
        let Ok(tty) = std::fs::File::open("/dev/tty") else {
            println!("\ndry run, no terminal to ask on. pass -y to restart.");
            return 0;
        };
        let n = plan.go.len();
        print!(
            "\nrestart {} session{}? [Y/n] ",
            n,
            if n == 1 { "" } else { "s" }
        );
        let _ = std::io::stdout().flush();
        let mut ans = String::new();
        if BufReader::new(tty).read_line(&mut ans).is_err()
            || !taimux_cli::restart::confirm_yes(&ans)
        {
            println!("nothing restarted.");
            return 0;
        }
    }
    println!();
    let (mut ok, mut bad) = (0, 0);
    for g in &plan.go {
        print!("restarting {} ({}) ... ", g.pane, g.target);
        let _ = std::io::stdout().flush();
        if !taimux_cli::restart::restart_pane(&g.pane, g.pid, &g.cmd) {
            println!("did not exit, left alone");
            bad += 1;
            continue;
        }
        // Wait for the replacement to come up, and say which version it is: the
        // whole point of the exercise is that it is the installed one.
        let mut newpid = None;
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(500));
            let Some(ppid) =
                taimux_core::tmux::ask(&["display-message", "-p", "-t", &g.pane, "#{pane_pid}"])
            else {
                break;
            };
            newpid = taimux_core::proc::children_of(ppid.trim().parse().unwrap_or(0))
                .into_iter()
                .find(|c| {
                    taimux_cli::restart::version_of_pid(*c, &taimux_cli::restart::versions_dir())
                        .is_some()
                });
            if newpid.is_some() {
                break;
            }
        }
        match newpid {
            Some(pid) => {
                println!(
                    "up on {} (pid {})",
                    taimux_cli::restart::version_of_pid(pid, &taimux_cli::restart::versions_dir())
                        .unwrap_or_default(),
                    pid
                );
                ok += 1;
            }
            None => {
                println!("exited but did not come back, check the pane");
                bad += 1;
            }
        }
    }
    println!("\n{} restarted, {} needing a look", ok, bad);
    if bad == 0 {
        0
    } else {
        1
    }
}

/// Move the attached client to a row: a local pane, a past conversation, or a
/// session on another host.
fn switch(id: &str) -> i32 {
    let id = id.to_string();
    if id.is_empty() {
        1 // nothing to switch to, and nothing said: same as bash
    } else if id == "dead:!" {
        0 // the "still building" note row: nothing to switch to
    } else if let Some((agent, key)) = taimux_core::index::split_past_id(&id) {
        let cwd = taimux_core::tmux::past_cwd(agent, key);
        match taimux_core::tmux::resume_dead(agent, key, &cwd) {
            Ok(()) => 0,
            Err(why) => {
                eprintln!("taimux: {}", why);
                1
            }
        }
    } else if id.starts_with('%') {
        let zoom = taimux_core::env::on("TAIMUX_ZOOM");
        if taimux_core::tmux::switch_local(&id, zoom) {
            0
        } else {
            1
        }
    } else if let Some(host) = taimux_cli::remote::pane_host(&id) {
        let zoom = taimux_core::env::on("TAIMUX_ZOOM");
        // The pane the host was DISCOVERED through is the way back to
        // it, and it is looked up before anything is moved: its absence
        // means it closed since the list was built, and there is now
        // nowhere to land.
        let local = taimux_cli::remote::ssh_tmux_panes(&taimux_cli::remote::tmux_pane_ttys())
            .into_iter()
            .find(|(h, _)| h == host)
            .map(|(_, p)| p)
            .unwrap_or_default();
        match taimux_cli::remote::switch_remote(
            host,
            taimux_cli::remote::pane_local(&id),
            &local,
            zoom,
        ) {
            Ok(()) => 0,
            Err(why) => {
                eprintln!("taimux: {}", why);
                1
            }
        }
    } else {
        eprintln!("taimux: {} is not a pane id", id);
        2
    }
}

/// The picker: resolve the pane it was opened from, fire the indexer behind it,
/// draw, and act on the answer.
///
/// One child for the whole session, which is the whole of what replaced fzf: no
/// reload per keystroke, no preview per cursor move, and no re-exec of anything
/// inside either of those.
fn pick() -> i32 {
    // The pane the picker was opened from, which is the row marked ●. Anything
    // that is not a pane id is treated as no answer at all and tmux is asked
    // directly: display-popup expands no format in the command it runs, so a
    // binding passing #{pane_id} either loses it to the shell (bare, where its
    // `#` opens a comment, which is why the binding `install` writes works) or
    // hands the picker the format ITSELF (quoted). Asking tmux from inside the
    // popup answers with the pane the client is on, which is the one you came
    // from.
    let arg = std::env::args().nth(2).unwrap_or_default();
    let cur = if arg.starts_with('%') && arg[1..].bytes().all(|b| b.is_ascii_digit()) {
        arg
    } else {
        taimux_core::tmux::ask(&["display-message", "-p", "#{pane_id}"]).unwrap_or_default()
    };
    // …and if that pane is a window onto another host, the row you are on is
    // over there, not the ssh pane you are looking through. Resolved once, here,
    // so the ● marker and the opening cursor position agree.
    let panes = taimux_cli::remote::tmux_pane_ttys();
    let cmd = taimux_core::tmux::ask(&[
        "display-message",
        "-p",
        "-t",
        &cur,
        "#{pane_current_command}",
    ])
    .unwrap_or_default();
    let cur = taimux_cli::remote::resolve_cur(&cur, &panes, cmd.trim());

    // The indexer, detached, so the picker never waits on it: coverage simply
    // arrives on a later refresh. Safe to fire on every open, because a pass
    // rate-limits itself against the same flock it holds.
    let search = taimux_core::env::on("TAIMUX_SEARCH");
    let sessions = taimux_core::env::on("TAIMUX_SESSIONS");
    if search || sessions {
        let exe = self_exe();
        let _ = Command::new("setsid")
            .arg(&exe)
            .arg("index")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .or_else(|_| {
                Command::new(&exe)
                    .arg("index")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
            });
    }

    let home = std::env::var("HOME").unwrap_or_default();
    let exe = self_exe();
    let src = taimux_cli::tui::Source {
        fetch: std::sync::Arc::new(move || {
            let mut p = taimux_core::version::Prober::new();
            let mut c = HashMap::new();
            let local = taimux_core::panes::list_rows(&mut p, &mut c);
            taimux_cli::remote::all_panes(&local, &taimux_cli::remote::tmux_pane_ttys())
        }),
        ended: taimux_cli::tui::ended_source(),
        cur: cur.clone(),
        newver: taimux_core::version::installed_claude(&home).unwrap_or_default(),
        home,
        script: Some(exe.clone()),
        // Only the binding can say this, with `-e TAIMUX_POPUP=1`.
        popup: taimux_core::env::is("TAIMUX_POPUP", "1"),
        state: restored_state(),
    };
    match taimux_cli::tui::run(src) {
        Ok(taimux_cli::tui::Outcome::Chosen(id)) => {
            // The jump goes here rather than through a caller, which is what
            // makes the picker one process: nothing has to read a pane id back
            // out of stdout and act on it.
            switch(&id)
        }
        Ok(taimux_cli::tui::Outcome::Aborted) => 0, // an ordinary abort
        // The terminal grew past this popup. tmux cannot resize one in place, so
        // the picker leaves and asks for a new one at the size the binding would
        // choose now, carrying what it was doing.
        Ok(taimux_cli::tui::Outcome::Resize(state)) => {
            reopen_popup(&exe, &cur, &state);
            0
        }
        Err(e) => {
            eprintln!("taimux: {}", e);
            1
        }
    }
}

/// The state a previous instance handed over, out of the environment its reopen
/// set. Empty for an ordinary open.
///
/// Through the environment rather than argv because the query is arbitrary text:
/// `display-popup -e VAR=value` takes it as one argument and never lets a shell
/// near it, while the command it runs is a string that would have to be quoted
/// by hand.
fn restored_state() -> taimux_cli::tui::State {
    let get = |k: &str| taimux_core::env::var(k).unwrap_or_default();
    taimux_cli::tui::State {
        query: get("TAIMUX_STATE_QUERY"),
        // Leaked into a &'static str, which is what Mode::key hands back and
        // what makes the struct free to build in the picker. One per process.
        mode: Box::leak(get("TAIMUX_STATE_MODE").into_boxed_str()),
        search: get("TAIMUX_STATE_SEARCH") == "1",
        // Absent means an ordinary open, where the preview is on.
        preview: taimux_core::env::var("TAIMUX_STATE_PREVIEW").is_none_or(|v| v == "1"),
        on: get("TAIMUX_STATE_ON"),
        // Only the picker that is resizing knows this, and it is passed to the
        // reopen directly rather than restored into the new one.
        client: String::new(),
    }
}

/// Ask tmux for a new popup at the geometry this client's width earns now.
///
/// Detached, and it waits a moment first: this process IS the popup, so the old
/// one is only gone once we exit, and tmux allows one popup per client. Retried,
/// because the whole cost of getting it wrong is the picker vanishing on a
/// resize.
fn reopen_popup(exe: &str, cur: &str, state: &taimux_cli::tui::State) {
    // The client the PICKER resolved, which is the one whose popup this is. An
    // untargeted `display-message` answers for whichever client tmux considers
    // current, and with a phone and a desktop both attached that is how a
    // reopened popup landed on the wrong one.
    let client = state.client.clone();
    if client.is_empty() {
        return;
    }
    // The state goes through a FILE, not the command line. `run-shell` takes one
    // shell string, and the query is arbitrary text that would have to be quoted
    // into it; a file keeps every character of it away from a shell.
    let path = taimux_core::paths::runtime_dir().join("resize-state");
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(
        &path,
        format!(
            "mode\t{}\nsearch\t{}\npreview\t{}\non\t{}\nquery\t{}\n",
            state.mode,
            state.search as u8,
            state.preview as u8,
            state.on,
            state.query.replace(['\n', '\r'], " "),
        ),
    );
    // `run-shell -b` and not a detached child of our own: tmux runs it itself,
    // after this popup is gone, which is the difference between a reopen that
    // lands and one tmux accepts and then discards. Measured both ways.
    let _ = taimux_core::tmux::run(&[
        "run-shell",
        "-b",
        &format!(
            "'{}' _repopup '{}' '{}'",
            sh_quote(exe),
            sh_quote(client.trim()),
            sh_quote(cur)
        ),
    ]);
}

/// A string as one single-quoted shell word.
///
/// Only ever used on paths and tmux's own ids here: the picker's query never
/// goes near a shell, it rides in the state file.
fn sh_quote(s: &str) -> String {
    s.replace('\'', "'\\''")
}

/// The other half of a resize: open the new popup once the old one is gone.
///
/// Its own entry point because it has to outlive the picker that asked for it,
/// and tmux is what runs it (see `reopen_popup`).
fn repopup(client: &str, cur: &str) -> i32 {
    // Read once and removed: a stale state file would otherwise restore an old
    // query onto the next ordinary open.
    let path = taimux_core::paths::runtime_dir().join("resize-state");
    let saved = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    let field = |k: &str| -> String {
        saved
            .lines()
            .find_map(|l| l.strip_prefix(k).and_then(|r| r.strip_prefix('\t')))
            .unwrap_or("")
            .to_string()
    };

    let width: usize =
        taimux_core::tmux::ask(&["display-message", "-c", client, "-p", "#{client_width}"])
            .and_then(|w| w.trim().parse().ok())
            .unwrap_or(0);
    if width == 0 {
        return 1; // the client went away with the resize; nothing to reopen onto
    }
    let (pw, ph) = taimux_cli::install::popup_geometry(width);
    let exe = self_exe();
    // Long enough for the popup we came out of to be gone. tmux allows one per
    // client, and a request that arrives while the old one is still closing is
    // accepted and then dropped, with tmux reporting success either way.
    std::thread::sleep(Duration::from_millis(250));
    for _ in 0..6 {
        let ok = Command::new("tmux")
            .args([
                "display-popup",
                "-c",
                client,
                "-w",
                &format!("{}%", pw),
                "-h",
                &format!("{}%", ph),
                "-E",
                "-e",
                "TAIMUX_POPUP=1",
                "-e",
                &format!("TAIMUX_STATE_QUERY={}", field("query")),
                "-e",
                &format!("TAIMUX_STATE_MODE={}", field("mode")),
                "-e",
                &format!("TAIMUX_STATE_SEARCH={}", field("search")),
                "-e",
                &format!("TAIMUX_STATE_PREVIEW={}", field("preview")),
                "-e",
                &format!("TAIMUX_STATE_ON={}", field("on")),
                &format!("'{}' pick {}", sh_quote(&exe), cur),
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return 0;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    // Never silent: the picker closed itself expecting to come back, and if it
    // did not, this log line is the only place that says why.
    taimux_cli::act::note("resize: could not reopen the picker's popup");
    1
}

/// This binary's own path, which is what a binding and a symlink both point at.
fn self_exe() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "taimux".into())
}

fn main() {
    // Keep mise off the network for every child this spawns.
    //
    // Reaching a tool through a mise shim drags mise's version RESOLUTION into
    // the popup, and a fuzzy pin it has never resolved blocks on api.github.com:
    // that is what left the picker as an empty popup twice, once on broken DNS
    // and once when GitHub was down. The picker itself is this binary now, so the
    // exposure is smaller than it was, but jq and claude can still be shims.
    // MISE_OFFLINE=0 opts back in.
    if std::env::var_os("MISE_OFFLINE").is_none() {
        std::env::set_var("MISE_OFFLINE", "1");
    }
    let arg = std::env::args().nth(1).unwrap_or_else(|| "help".into());
    let rc = match arg.as_str() {
        "serve" => taimux_daemon::protocol::serve()
            .map(|_| 0)
            .unwrap_or_else(|e| {
                eprintln!("taimux: {}", e);
                1
            }),
        "panes" | "ping" => taimux_daemon::protocol::query(&arg).map(|_| 0).unwrap_or(1),
        // **The wire format another host answers with over ssh**, so it has to
        // work with nothing else running: a host is not required to have a
        // daemon up, and for most of them none ever will be.
        //
        // A daemon is asked FIRST when there is one, because it has the captures
        // cached and the answer is identical either way. Getting this wrong broke
        // federation silently: `list` meaning "ask a daemon" exits 1 where none
        // is listening, so every remote host answered nothing and simply vanished
        // from the list.
        //
        // `list-local` is the same thing with the daemon never asked, which is
        // what the differentials and the golden replay want.
        "list" => {
            match taimux_daemon::protocol::ask("list") {
                Some(body) => print!("{}", body),
                None => {
                    let mut p = taimux_core::version::Prober::new();
                    let mut c = HashMap::new();
                    print!("{}", taimux_core::panes::list_rows(&mut p, &mut c));
                }
            }
            0
        }
        "list-local" => {
            let mut p = taimux_core::version::Prober::new();
            let mut c = HashMap::new();
            print!("{}", taimux_core::panes::list_rows(&mut p, &mut c));
            0
        }
        // No daemon needed: the same scan, run in-process. This is what makes the
        // crate testable against the live machine without a socket, and what the
        // bash suite can diff against.
        "scan" => {
            print!("{}", taimux_core::panes::agent_rows());
            0
        }
        // The turn-boundary hook a session runs about itself. No socket: see
        // hook.rs. Always exits 0, because a hook that fails must never be
        // visible to the session it is reporting on.
        "hook" => taimux_core::hook::run(taimux_core::paths::runtime_dir()),
        // A transcript's prose, for diffing against the bash extractor. The
        // equivalence bar for stage 4 is the same as every other stage: identical
        // bytes out of the same file.
        "extract" => match std::env::args().nth(2) {
            Some(path) => match std::fs::read_to_string(&path) {
                Ok(text) => {
                    print!("{}", taimux_core::transcript::extract(&text));
                    0
                }
                Err(e) => {
                    eprintln!("taimux: {}: {}", path, e);
                    1
                }
            },
            None => {
                let mut buf = String::new();
                let _ = std::io::stdin().read_to_string(&mut buf);
                print!("{}", taimux_core::transcript::extract(&buf));
                0
            }
        },
        // Classify a screen read on stdin. Exists for differential testing: the
        // only way to tell a porting bug from a session that simply changed
        // between two captures is to feed both implementations the same text.
        "classify" => {
            let mut buf = String::new();
            let _ = std::io::stdin().read_to_string(&mut buf);
            println!("{}", taimux_core::state::classify(&buf).as_str());
            0
        }
        // Step 1 of removing fzf: a native list, drawn here, over real rows. It
        // answers whether the alternate screen, bracketed paste and resize behave
        // inside `tmux display-popup -E`; see tui.rs. Rows come from a running
        // daemon when there is one and are scanned here when there is not, so the
        // spike works either way.
        "tui" => {
            let home = std::env::var("HOME").unwrap_or_default();
            // The script, when the picker was launched from it. It is also the
            // row source: this server's panes are ours to scan, but every
            // DISCOVERED host's are not, and the ssh, the discovery and the
            // caching behind those all live in bash. Asking it for the merged
            // list is one fork per refresh and keeps federation in one place.
            let script = taimux_core::env::var("TAIMUX_SELF").filter(|s| !s.is_empty());
            let via = script.clone();
            let src = taimux_cli::tui::Source {
                // Asked again on every refresh, so a daemon that starts (or dies)
                // while the picker is open is picked up at the next tick rather
                // than at the next launch.
                fetch: std::sync::Arc::new(move || {
                    if let Some(s) = &via {
                        if let Ok(o) = Command::new(s).arg("_panes").output() {
                            if o.status.success() {
                                return String::from_utf8_lossy(&o.stdout).into_owned();
                            }
                        }
                    }
                    // Standalone, or the script refused: this server only.
                    taimux_daemon::protocol::ask("list").unwrap_or_else(|| {
                        let mut p = taimux_core::version::Prober::new();
                        let mut c = HashMap::new();
                        taimux_core::panes::list_rows(&mut p, &mut c)
                    })
                }),
                ended: taimux_cli::tui::ended_source(),
                cur: std::env::args().nth(2).unwrap_or_default(),
                // What a session started right now would run, so a pane a
                // self-update has left behind is painted yellow: that is the row
                // ctrl-x acts on.
                newver: taimux_core::version::installed_claude(&home).unwrap_or_default(),
                home,
                // Unset standalone, and the restart keys are then simply not
                // bound: the header only says what is really there.
                script,
                // This entry point is what the tests drive and what a standalone
                // run uses, neither of which is a popup the binding opened.
                popup: false,
                state: Default::default(),
            };
            match taimux_cli::tui::run(src) {
                Ok(taimux_cli::tui::Outcome::Chosen(id)) => {
                    println!("{}", id);
                    0
                }
                Ok(taimux_cli::tui::Outcome::Resize(_)) | Ok(taimux_cli::tui::Outcome::Aborted) => {
                    130
                } // fzf's abort status, and the picker's
                Err(e) => {
                    eprintln!("taimux: {}", e);
                    1
                }
            }
        }
        // One pane's transcript, indexed. The incremental behaviour is the whole
        // reason this is its own entry point: a grown transcript must be read
        // only from where the last pass stopped, a shrunken one is not the same
        // file at all, and a pane moved onto another conversation must be rebuilt
        // rather than appended to. Those are hard to see from a whole pass and
        // easy to assert one file at a time.
        "index-pane" => {
            let a: Vec<String> = std::env::args().skip(2).collect();
            match (a.first(), a.get(1)) {
                (Some(id), Some(tr)) => {
                    taimux_daemon::indexer::index_pane(id, std::path::Path::new(tr));
                    0
                }
                _ => {
                    eprintln!("taimux: index-pane <pane id> <transcript>");
                    1
                }
            }
        }
        // One conversation's own account of itself: cwd, version, title, and
        // whether that title is real or a last prompt standing in for one. Read
        // BACKWARDS and stopped early, which is what makes hundreds of them
        // affordable, so it is worth being able to assert on one file.
        "session-meta" => match std::env::args().nth(2) {
            Some(path) => {
                // The agent is a flag rather than guessed from the path, and it
                // defaults to claude, which is what this entry point was for
                // before there were four others.
                let a: Vec<String> = std::env::args().skip(2).collect();
                let agent = a
                    .iter()
                    .position(|x| x == "--agent")
                    .and_then(|i| a.get(i + 1))
                    .cloned()
                    .unwrap_or_else(|| "claude".into());
                let m = taimux_core::agents::meta(&agent, &path);
                println!("{}\t{}\t{}\t{}", m.cwd, m.version, m.title, m.src);
                0
            }
            None => {
                eprintln!("taimux: session-meta <transcript>");
                1
            }
        },
        // The sessions cache and the prune, on their own, for the same reason.
        // The live map arrives on stdin as `<pane> \t <transcript>` lines, exactly
        // as it did in bash, and for the same reason: resolving which pane is on
        // which conversation is the expensive part of a pass, so it is done ONCE
        // and handed in rather than recomputed here.
        "index-sessions" => {
            let mut buf = String::new();
            let _ = std::io::stdin().read_to_string(&mut buf);
            let live: Vec<(String, PathBuf)> = buf
                .lines()
                .filter_map(|l| l.split_once('\t'))
                .filter(|(_, p)| !p.is_empty())
                .map(|(pane, p)| (pane.to_string(), PathBuf::from(p)))
                .collect();
            taimux_daemon::indexer::sessions_scan(&live);
            0
        }
        "index-dead" => {
            taimux_daemon::indexer::index_dead();
            0
        }
        "index-prune" => {
            taimux_daemon::indexer::prune();
            0
        }
        // Rewrite a tmux-resurrect save so each claude pane comes back on the
        // conversation it was having. `-n` diffs instead of writing.
        "resurrect" => {
            let a: Vec<String> = std::env::args().skip(2).collect();
            let (mut dry, mut verbose, mut file) = (false, false, String::new());
            let mut bad = None;
            for x in &a {
                match x.as_str() {
                    "-n" | "--dry-run" => dry = true,
                    "-v" | "--verbose" => verbose = true,
                    o if o.starts_with('-') => bad = Some(o.to_string()),
                    o => file = o.to_string(),
                }
            }
            if let Some(b) = bad {
                eprintln!("resurrect: unknown option {}", b);
                1
            } else {
                if file.is_empty() {
                    file = format!(
                        "{}/.tmux/resurrect/last",
                        std::env::var("HOME").unwrap_or_default()
                    );
                }
                resurrect_main(&file, dry, verbose)
            }
        }
        // The restart plan, and optionally carrying it out. `-n` prints the plan
        // and stops, which is the shape the equivalence bar is measured on: what
        // it decided, and why, for every pane.
        "restart" => {
            let a: Vec<String> = std::env::args().skip(2).collect();
            let mut o = taimux_cli::restart::Opts {
                include_busy: false,
                only_panes: Vec::new(),
                force_transcript: None,
                self_pane: std::env::var("TMUX_PANE").unwrap_or_default(),
            };
            let (mut go, mut ask) = (false, true);
            let mut i = 0;
            let mut bad: Option<String> = None;
            while i < a.len() {
                match a[i].as_str() {
                    "-y" | "--yes" => go = true,
                    "-n" | "--dry-run" => {
                        go = false;
                        ask = false;
                    }
                    "--include-busy" => o.include_busy = true,
                    "--pane" => {
                        i += 1;
                        o.only_panes.push(a.get(i).cloned().unwrap_or_default());
                    }
                    "--transcript" => {
                        i += 1;
                        o.force_transcript = a.get(i).cloned();
                    }
                    other => bad = Some(other.to_string()),
                }
                i += 1;
            }
            if let Some(b) = bad {
                eprintln!("restart: unknown option {}", b);
                1
            } else {
                restart_main(&o, go, ask)
            }
        }
        // One record per claude pane: whether its conversation could be
        // identified, why, and the command that would put it back. What
        // `resurrect` rewrites a save file from and what `restart` decides from.
        "print-cmds" => {
            // The state is merged the same way the list merges it, so a record
            // reports the screen-and-hook reading (input / run / idle) rather
            // than the title glyph: one busy taxonomy in the tool instead of two
            // that drift apart, which is what the ccrestart absorption was for.
            let state_of = |id: &str, pid: i32| -> String {
                let screen = taimux_core::tmux::capture(id).unwrap_or_default();
                taimux_core::state::merge(
                    &screen,
                    taimux_core::hook::hook_entry(id, pid)
                        .as_ref()
                        .map(|(st, _)| st.as_str()),
                )
                .as_str()
                .to_string()
            };
            print!(
                "{}",
                taimux_core::conv::print_cmds(&taimux_core::panes::agent_rows(), &state_of)
            );
            0
        }
        // The picker's row source: this server's agent panes PLUS every
        // discovered host's. Federation lives here now, and the rule that keeps
        // it safe is unchanged: each host is asked about ITSELF, and `list` stays
        // local-only because it is the wire format those hosts answer with.
        "all-panes" => {
            let mut p = taimux_core::version::Prober::new();
            let mut c = HashMap::new();
            let local = taimux_core::panes::list_rows(&mut p, &mut c);
            print!(
                "{}",
                taimux_cli::remote::all_panes(&local, &taimux_cli::remote::tmux_pane_ttys())
            );
            0
        }
        // The pane the picker was opened from, resolved THROUGH an ssh window: if
        // that pane is a window onto another host, the row you are on is over
        // there, not the ssh pane you are looking through.
        "resolve-cur" => {
            let pane = std::env::args().nth(2).unwrap_or_default();
            let cmd = taimux_core::tmux::ask(&[
                "display-message",
                "-p",
                "-t",
                &pane,
                "#{pane_current_command}",
            ])
            .unwrap_or_default();
            println!(
                "{}",
                taimux_cli::remote::resolve_cur(
                    &pane,
                    &taimux_cli::remote::tmux_pane_ttys(),
                    cmd.trim()
                )
            );
            0
        }
        // This host's index, for another host to read. LOCAL panes only, which is
        // the same rule: a host must never answer with another host's rows.
        "index-dump" => {
            print!("{}", taimux_cli::remote::index_dump());
            0
        }
        // A pane rendered for the preview, in the exact bytes the bash version
        // printed: this is also the wire format another host answers with over
        // ssh, so it is not ours alone to change.
        //
        // A REMOTE id is not handled here. capture-pane only works where the pane
        // is, and the ssh, the host resolution and the bound it runs under all
        // still live in the script.
        "preview" => {
            let id = std::env::args().nth(2).unwrap_or_default();
            let q = std::env::args().nth(3).unwrap_or_default();
            if id.is_empty() {
                0
            } else if id.starts_with("dead:") {
                print!("{}", taimux_core::tmux::preview_dead(&id, &q));
                0
            } else if id.starts_with('%') {
                print!("{}", taimux_core::tmux::preview_live(&id, &q));
                0
            } else if let Some(host) = taimux_cli::remote::pane_host(&id) {
                // Another host renders its own pane: capture-pane only works
                // where the pane is. A host that stops answering between the
                // list and the cursor landing on its row says so here rather
                // than leaving the pane blank.
                println!("\x1b[1;35m{}\x1b[0m", host);
                let lid = taimux_cli::remote::pane_local(&id);
                if lid.starts_with('%') {
                    let (rc, out) = taimux_cli::remote::ssh(
                        host,
                        &format!("{} preview {}", taimux_cli::remote::remote_taimux(), lid),
                        taimux_core::env::var("TAIMUX_SSH_TIMEOUT")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(4),
                    );
                    if rc == 0 {
                        print!("{}", out);
                    } else {
                        println!("\x1b[31mno answer from {}\x1b[0m", host);
                    }
                } else {
                    println!(
                        "\x1b[90mnothing to show: this row is about the host, not a session\x1b[0m"
                    );
                }
                0
            } else {
                0
            }
        }
        // Move the attached client to a pane, or reopen an ended conversation.
        // Same restriction: a remote id belongs to the script.
        "switch" => switch(&std::env::args().nth(2).unwrap_or_default()),
        // The transcript a session id names, or nothing when it is ambiguous. A
        // prefix is all `claude attach` asks you to type, so it can be short
        // enough to name two conversations, and resuming the wrong one is what
        // the whole ladder exists to avoid.
        "transcript-by-id" => match std::env::args()
            .nth(2)
            .and_then(|id| taimux_core::conv::transcript_by_id(&id))
        {
            Some(p) => {
                println!("{}", p.display());
                0
            }
            None => 1,
        },
        // Putting taimux on PATH and binding the key, and the plugin entry
        // point's own `bind`, which writes nothing.
        "install" => taimux_cli::install::install(&self_exe()),
        "bind" => taimux_cli::install::bind(&self_exe()),
        "install-hooks" => taimux_cli::install::install_hooks(&self_exe()),
        // The picker: resolve the pane it was opened from, fire the indexer
        // behind it, draw, and act on the answer. One child for the whole
        // session, which is the whole of what replaced fzf.
        "pick" => {
            if std::env::var("TMUX").map(|v| v.is_empty()).unwrap_or(true) {
                eprintln!("Not inside tmux: run this from a tmux client (or use prefix + a).");
                1
            } else {
                pick()
            }
        }
        // The picker's two acting keys. Both own the screen the picker hands
        // over, and both end in a DETACHED restart: a restart waits up to twelve
        // seconds for a session to exit and then polls for it to come back, so
        // run inline it would freeze the popup for half a minute.
        "_restart" => {
            taimux_cli::act::restart_one(&self_exe(), &std::env::args().nth(2).unwrap_or_default());
            0
        }
        "_sweep" => {
            taimux_cli::act::sweep(&self_exe());
            0
        }
        "_handoff" => {
            taimux_cli::act::handoff_one(&std::env::args().nth(2).unwrap_or_default());
            0
        }
        // The handoff without the picker: what ctrl-o builds, for a script, and
        // for seeing what a target would actually be started with.
        //
        //   taimux handoff <row id> --print
        //   taimux handoff <row id> --to codex
        //
        // A row id is `dead:<agent>:<key>` (what `dead-rows` prints) or a local
        // claude pane id.
        "handoff" => handoff_main(&std::env::args().skip(2).collect::<Vec<_>>()),
        // The second half of a resize, run detached by the picker it replaces.
        "_repopup" => {
            let a: Vec<String> = std::env::args().skip(2).collect();
            match (a.first(), a.get(1)) {
                (Some(client), cur) => repopup(client, cur.map(String::as_str).unwrap_or("")),
                _ => {
                    eprintln!("taimux: _repopup <client tty> [pane]");
                    1
                }
            }
        }
        // Which conversation a pane is on, and which rung of the ladder said so.
        // Exists for the differential: the bash side prints the same two fields
        // from $R_TRANSCRIPT and $R_WHY.
        "resolve" => {
            let a: Vec<String> = std::env::args().skip(2).collect();
            let (pane, cwd) = (
                a.first().cloned().unwrap_or_default(),
                a.get(1).cloned().unwrap_or_default(),
            );
            let pid: i32 = a.get(2).and_then(|p| p.parse().ok()).unwrap_or(0);
            match taimux_core::conv::resolve_from_pane(&pane, &cwd, pid) {
                Some(r) => {
                    println!("{}\t{}", r.transcript.display(), r.why);
                    0
                }
                None => 1,
            }
        }
        // One indexer pass: what each live session has said since the last one,
        // plus the cache of every conversation on disk.
        //
        // Safe to run as often as anything likes, which is the point. The lock is
        // a real flock, so a second pass cannot start inside the first one's tail
        // and the kernel releases it if the holder dies; the same file's mtime is
        // the rate limit, so a pass that has just run is skipped. In bash this was
        // a lock DIRECTORY with a steal-it-if-old rule plus a separate stamp file,
        // and the fork bomb defeated the stamp by giving every generation a fresh
        // runtime directory to keep it in.
        //
        // --force runs regardless of the rate limit (still under the lock).
        "index" => {
            let search = taimux_core::env::on("TAIMUX_SEARCH");
            let sessions = taimux_core::env::on("TAIMUX_SESSIONS");
            if !search && !sessions {
                0
            } else {
                let lockfile = taimux_core::index::index_dir().join(".lock");
                match taimux_daemon::indexer::Lock::take(&lockfile) {
                    None => 0, // somebody else is doing it, which is the answer
                    Some(mut lock) => {
                        let ttl = taimux_core::env::var("TAIMUX_SEARCH_TTL")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(5u64);
                        let forced = std::env::args().any(|a| a == "--force");
                        if !forced && lock.age() < ttl {
                            0
                        } else {
                            lock.stamp();
                            // agent_rows, NOT list_rows: the indexer needs the
                            // agent PID to read a pane's argv, and only the
                            // seven-field scan carries it. The eight-field list
                            // has the version in that column, which parses as pid
                            // 0, and the only pane that then failed to resolve was
                            // the one with no pane map to fall back on.
                            taimux_daemon::indexer::pass(
                                &taimux_core::panes::agent_rows(),
                                search,
                                sessions,
                            );
                            // …and every discovered host's index, on its own
                            // longer TTL: what was said in a session does not go
                            // stale on the timescale a pane's state does.
                            if search {
                                taimux_cli::remote::index_remote(
                                    &taimux_cli::remote::tmux_pane_ttys(),
                                );
                            }
                            0
                        }
                    }
                }
            }
        }
        // What each session SAID that matches a query: `<pane> \t <snippet>`, one
        // per line. Its own entry point because the index is written by bash and
        // read here, so that format is a contract between two implementations and
        // wants a test that crosses it.
        "snips" => {
            let q = std::env::args().skip(2).collect::<Vec<_>>().join(" ");
            let mut out: Vec<(String, String)> =
                taimux_core::index::snippets(&taimux_core::index::Query::new(&q))
                    .into_iter()
                    .collect();
            out.sort(); // a directory read is in no particular order
            for (pane, snip) in out {
                println!("{}\t{}", pane, snip);
            }
            0
        }
        // The ended-sessions list, off the sessions cache, in the same eight
        // fields a pane row has. Its own entry point because it is what the bash
        // suite asserts the format against now that `_dead_rows` is gone.
        "dead-rows" => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            print!("{}", taimux_core::index::dead_rows(now));
            0
        }
        // The laid-out picker rows, in the exact bytes the awk prints, read from
        // the same TSV the awk reads. This is the whole equivalence bar for step
        // 2: no scan, no tmux, nothing that can differ between two runs, just the
        // layout fed identical input on both sides.
        "rows" => {
            let a: Vec<String> = std::env::args().skip(2).collect();
            let flag = |name: &str| -> Option<String> {
                a.iter()
                    .position(|x| x == name)
                    .and_then(|i| a.get(i + 1))
                    .cloned()
            };
            let pairs = |var: &str| -> HashMap<String, String> {
                taimux_core::env::var(var)
                    .unwrap_or_default()
                    .lines()
                    .filter_map(|l| l.split_once('\t'))
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect()
            };
            let (cur, home, newver, only) = (
                flag("--cur").unwrap_or_default(),
                flag("--home").unwrap_or_default(),
                flag("--newver").unwrap_or_default(),
                flag("--only").unwrap_or_default(),
            );
            let query = taimux_core::env::var("TAIMUX_Q").unwrap_or_default();
            let mut buf = String::new();
            let _ = std::io::stdin().read_to_string(&mut buf);
            let input = taimux_cli::rows::Input {
                cur: &cur,
                width: flag("--width").and_then(|w| w.parse().ok()).unwrap_or(0),
                home: &home,
                newver: &newver,
                only: &only,
                // What the picker's outdated stop passes, so this entry point can
                // still express every list the picker can draw.
                outdated: a.iter().any(|x| x == "--outdated"),
                query: &query,
                snips: pairs("TAIMUX_SNIPS"),
                ptitles: pairs("TAIMUX_PTITLES"),
                // Always empty here. This entry point lays out a TSV handed to it
                // on stdin, with no picker and no restart in flight, and it is
                // what the golden file is diffed against: a marker that only a
                // live picker can produce has no business changing that baseline.
                restarting: Default::default(),
            };
            for row in taimux_cli::rows::build(&buf, &input) {
                println!("{}", row.to_ansi());
            }
            0
        }
        // The full list, in-process, no daemon: what the bash suite diffs against.
        "scan-list" => {
            let mut p = taimux_core::version::Prober::new();
            let mut c = HashMap::new();
            print!("{}", taimux_core::panes::list_rows(&mut p, &mut c));
            0
        }
        // Load-bearing, not decoration: the installer reads it to decide whether
        // the release it just looked up is already the one on disk, so a
        // `chezmoi apply` on a host that is current downloads nothing. It comes
        // from Cargo.toml, which release-please bumps, so the tag, the crate and
        // this string cannot drift apart.
        "version" | "--version" | "-V" => {
            println!("taimux {}", env!("CARGO_PKG_VERSION"));
            0
        }
        "help" | "--help" | "-h" => {
            print!("{}", help());
            0
        }
        _ => {
            eprintln!("taimux: no such command: {}\n", arg);
            eprint!("{}", help());
            1
        }
    };
    std::process::exit(rc);
}

/// What the tool answers `help` with. One place, because the command list is
/// long enough now that two copies would drift.
fn help() -> String {
    format!(
        "taimux {}, a picker for live AI coding-agent sessions\n\
         \n\
         pick            the picker (what prefix+a and F1 run)\n\
         switch <id>     jump to a pane id, local or <host>:<pane>\n\
         list            the agent sessions here, tab-separated\n\
         preview <id>    what a session's pane, or transcript, is showing\n\
         handoff <id>    carry a conversation into a different agent\n\
         \n\
         print-cmds      which conversation each claude pane is on\n\
         restart [-n]    restart idle claude panes on their own conversation\n\
         resurrect       rewrite a tmux-resurrect save to resume conversations\n\
         \n\
         index [--force] one indexing pass: transcripts, sessions, past rows\n\
         hook            a session reporting its own turn boundary\n\
         serve           the collection daemon (socket: {})\n\
         \n\
         install         symlink the launcher and bind prefix+a and F1\n\
         bind            bind a running tmux server, by absolute path\n\
         install-hooks   register the self-reporting hook with Claude Code\n\
         version         print the version\n",
        env!("CARGO_PKG_VERSION"),
        taimux_core::paths::socket_path().display()
    )
}
