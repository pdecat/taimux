//! The two keys that act rather than navigate: ctrl-x on a row, f8 on the list.
//!
//! Both own the whole screen for as long as they are up, because the picker has
//! just handed it over, and both END in a detached restart: a restart waits up to
//! twelve seconds for a session to exit and then polls for it to come back, so
//! run inline it would freeze the popup for half a minute.
//!
//! The shape of each is the same and it is deliberate: **say what will happen,
//! ask, and never offer something that would be refused anyway.** A restart that
//! declines is not a failure to explain away, it is the guard working.

use std::io::{Read, Write};

use crate::{remote, restart};

/// Own the screen the picker just handed over.
fn clear(out: &mut impl Write) {
    let _ = write!(out, "\x1b[H\x1b[2J");
}

/// What the terminal answered when a key was asked for.
///
/// The two cases used to be one `None`, and conflating them is why a screen
/// could vanish before it was read. A key that cannot be waited for is not a
/// key that was pressed: with no readable `/dev/tty` the read returns at once,
/// `any_key` returns at once, and the caller's `resume()` wipes the screen in
/// the same breath. Nothing anywhere said the pause had not happened.
enum Pressed {
    Key(char),
    /// No `/dev/tty` to open, or it answered EOF or an error. **Nothing waited.**
    Unreadable,
}

/// Wait for any single key, on the terminal rather than on stdin: the picker's
/// stdout is where the answer goes, and a popup has no stdin worth reading.
fn any_key() {
    print!("  Press any key…");
    let _ = std::io::stdout().flush();
    if let Pressed::Unreadable = read_one_key() {
        // Say so rather than racing past. This screen is about to be wiped by
        // whoever handed the terminal over, so an unread message is the same as
        // no message, and "it flashed and went" is exactly how this was
        // reported.
        println!("\r\n  (no readable terminal to wait on, so this was not held)");
        let _ = std::io::stdout().flush();
    }
}

fn read_one_key() -> Pressed {
    let Ok(mut tty) = std::fs::File::open("/dev/tty") else {
        return Pressed::Unreadable;
    };
    // Raw mode, or the read waits for a newline and "any key" becomes "Enter".
    let raw = crossterm::terminal::enable_raw_mode().is_ok();
    let mut b = [0u8; 1];
    let got = match tty.read(&mut b) {
        Ok(1) => Pressed::Key(b[0] as char),
        // 0 is EOF and an Err is a terminal we cannot read; neither is a press.
        _ => Pressed::Unreadable,
    };
    if raw {
        let _ = crossterm::terminal::disable_raw_mode();
    }
    got
}

/// Report a child that could not be started, and hold the screen while it is
/// read.
///
/// The caller has already left the alternate screen, so this lands where the
/// child would have drawn, and the pause is what stops `resume()` wiping it.
/// Without this the whole failure was invisible: the picker suspended, the
/// child never ran, the picker resumed, and the only evidence was a flicker.
///
/// **Written to `/dev/tty`, never to stdout.** Everything else in this file is
/// printed by a CHILD, whose stdout `act_child` has already pointed at the
/// terminal. This one runs in the PICKER, whose stdout carries exactly one
/// thing, the chosen pane id. Printing it cost the first attempt at this fix:
/// the report went down the pipe to whoever called the picker, the screen
/// stayed blank, and the test caught it.
pub fn report_failed_child(what: &str, e: &std::io::Error) {
    let Ok(mut tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") else {
        return;
    };
    // \r\n throughout: the picker may still have the terminal in raw mode when
    // this runs, where a bare \n moves down without returning to column 0 and
    // the message walks off to the right.
    let _ = write!(
        tty,
        "\x1b[H\x1b[2J\x1b[1mtaimux: could not run {}\x1b[0m\r\n\r\n  {}\r\n\r\n\
         \x20 The picker re-runs its OWN binary for this, so the usual cause is\r\n\
         \x20 that binary moving or being rebuilt underneath a running picker.\r\n\
         \x20 Closing and reopening the picker picks up the new one.\r\n\r\n\
         \x20 Press any key…",
        what, e
    );
    let _ = tty.flush();
    let _ = read_one_key();
}

/// The pane lines of a plan, without its detail lines or its skip list.
///
/// The detail lines are indented further and the skip list sits past the
/// `skipped (` heading, so this is a state machine over two headings rather than
/// a guess at indentation.
fn plan_panes(plan: &str) -> Vec<&str> {
    let mut on = false;
    let mut out = Vec::new();
    for l in plan.lines() {
        if l.starts_with("to restart (") {
            on = true;
            continue;
        }
        if l.starts_with("skipped (") {
            on = false;
        }
        if on && l.starts_with("  %") {
            out.push(l);
        }
    }
    out
}

fn count_in(plan: &str, heading: &str) -> Option<usize> {
    plan.lines()
        .find(|l| l.starts_with(heading))
        .and_then(|l| l.split(['(', ')']).nth(1))
        .and_then(|n| n.parse().ok())
}

/// The one line of a plan that is about this pane.
fn why_for(plan: &str, pane: &str) -> Option<String> {
    let needle = format!("  {} ", pane);
    plan.lines()
        .find(|l| l.contains(&needle))
        .map(|l| l.trim_start().to_string())
}

/// Fire a restart and return AT ONCE, appending to the log beside everything
/// else this keeps in the runtime directory.
///
/// Detached, so closing the popup (or picking a pane and leaving) cannot SIGHUP a
/// restart mid-flight and leave a pane with a dead session and nothing typed
/// into it. Nothing is printed: the picker owns the screen, and a detached job
/// has nowhere to print anyway.
pub fn note(line: &str) {
    let log = taimux_core::paths::runtime_dir().join("restart.log");
    if let Some(d) = log.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
    {
        let _ = writeln!(f, "--- {} {}", taimux_core::log::stamp(), line);
    }
}

/// The stdio a DETACHED job must have, and the reason it is a function.
///
/// stdout and stderr go to the log, which was always so. stdin is the one that
/// was missing, and Rust inherits it: `Command::spawn` defaults every stream to
/// the parent's, so a restart fired from the picker kept the POPUP'S pty open
/// on fd 0 long after the picker had exited. tmux then had a popup whose
/// command was gone but whose terminal still had a holder, which is a popup
/// that sits there blank, echoing whatever is typed at it, until the restart
/// finally ends and lets go. Reported exactly that way: blank after F8,
/// confirm, then Enter, and gone the moment every agent had come back.
///
/// It is also the definition of detaching. A job that outlives the thing that
/// started it must not hold that thing's terminal.
fn detached(
    cmd: &mut std::process::Command,
    out: std::fs::File,
    err: std::fs::File,
) -> &mut std::process::Command {
    cmd.stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err)
}

pub fn restart_detached(exe: &str, pane: &str, force: bool) {
    let log = taimux_core::paths::runtime_dir().join("restart.log");
    note(if pane.is_empty() {
        "all outdated panes"
    } else {
        pane
    });
    let mut args: Vec<String> = vec!["restart".into(), "-y".into()];
    if pane.starts_with('%') {
        args.push("--pane".into());
        args.push(pane.into());
    }
    if force {
        args.push("--include-busy".into());
    }
    let Ok(out) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
    else {
        return;
    };
    let Ok(err) = out.try_clone() else { return };
    let Ok(out2) = out.try_clone() else { return };
    // setsid so it survives the popup closing. `Command` cannot setsid without
    // libc, so this goes through the program, and falls back to a plain spawn
    // where there is none: a restart that dies with the popup is still better
    // than no restart, and the log says which happened.
    let spawned = detached(
        std::process::Command::new("setsid").arg(exe).args(&args),
        out2,
        err,
    )
    .spawn();
    if spawned.is_err() {
        let Ok(out3) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
        else {
            return;
        };
        let Ok(err2) = out3.try_clone() else { return };
        let _ = detached(std::process::Command::new(exe).args(&args), out3, err2).spawn();
    }
}

/// ctrl-x: restart the highlighted session.
pub fn restart_one(exe: &str, pane: &str) {
    let mut out = std::io::stdout();

    // An ended conversation has nothing to restart: there is no process behind
    // it and no pane to type into. Enter is the key that acts on those rows, and
    // saying so beats a keypress that looks like it did nothing.
    if pane.starts_with("dead:") {
        clear(&mut out);
        println!("\x1b[1mtaimux: that session has already ended\x1b[0m\n");
        println!("  There is nothing running to restart. Press Enter on it instead:");
        println!("  it opens again in a new window, in its own directory.\n");
        any_key();
        return;
    }

    // Another host's session. A restart reads /proc, resolves a transcript under
    // ~/.claude and sends keys to a tmux pane, all of which have to happen where
    // the session is, and that host's own taimux does all three. Rather than
    // half a feature, the key says so and hands over the line that would do it.
    if let Some(host) = remote::pane_host(pane) {
        clear(&mut out);
        println!(
            "\x1b[1mtaimux: {} is on {}\x1b[0m\n",
            remote::pane_local(pane),
            host
        );
        println!("  Restarting is local-only. From that host, or from here:\n");
        println!(
            "    ssh {} taimux restart --pane {}\n",
            host,
            remote::pane_local(pane)
        );
        any_key();
        return;
    }
    if !pane.starts_with('%') {
        return; // not a pane id: nothing to act on
    }

    let plan = plan_of(exe, &["-n", "--pane", pane]);
    if plan.contains("\nto restart (") || plan.starts_with("to restart (") {
        restart_detached(exe, pane, false);
        return;
    }

    clear(&mut out);
    println!("\x1b[1mtaimux: {} will not restart cleanly\x1b[0m\n", pane);
    let Some(why) = why_for(&plan, pane) else {
        println!("  Nothing to do: it is already on the installed version, or it is");
        println!("  not a claude pane.\n");
        any_key();
        return;
    };
    println!("  {}", why);

    // Never offer a force that would be refused anyway: an unresolved
    // conversation, this very pane, and a transcript already claimed by another
    // pane are all untouched by --include-busy.
    let forced = plan_of(exe, &["-n", "--pane", pane, "--include-busy"]);
    if !(forced.contains("\nto restart (") || forced.starts_with("to restart (")) {
        println!(
            "\n  Forcing would not help:\n  {}",
            why_for(&forced, pane).unwrap_or_else(|| "same refusal".into())
        );
        println!();
        any_key();
        return;
    }

    println!("\n  Forcing accepts losing an in-flight turn. A pane holding a");
    println!("  permission dialog is still refused, so nothing gets answered for you.");
    print!("\n\x1b[1mForce the restart?\x1b[0m [Y/n] ");
    let _ = out.flush();
    // An unreadable terminal declines, which is the safe default, but it SAYS
    // so: a silent "no" here is indistinguishable from the user answering n,
    // and the two want very different things done about them.
    let go = match read_one_key() {
        Pressed::Key(c) => restart::confirm_yes(&c.to_string()),
        Pressed::Unreadable => {
            print!("\r\n  (no readable terminal to answer on, so nothing was restarted)");
            false
        }
    };
    println!();
    if go {
        restart_detached(exe, pane, true);
        println!(
            "\n  Forced, detached. Watch the version, or read\n  {}",
            taimux_core::paths::runtime_dir()
                .join("restart.log")
                .display()
        );
        std::thread::sleep(std::time::Duration::from_millis(1200));
    } else {
        println!("\n  Left alone.");
        std::thread::sleep(std::time::Duration::from_millis(600));
    }
}

/// f8: restart every outdated session, after showing the plan and asking.
///
/// "outdated" throughout, which is the word the picker's own list of those rows
/// uses: the border label, the header hint and this screen are all reached in
/// one keypress of each other, and two words for one thing there read as two
/// different things.
pub fn sweep(exe: &str) {
    let mut out = std::io::stdout();
    // Timed, and written down when it fires. A sweep is the one press that can
    // leave the picker with real work to do afterwards, and when it was reported
    // as a freeze there was no record of how long any phase took, so every
    // theory about it stayed a theory. The picker writes its own slow refreshes
    // to this same log, which is what lets the two halves be read as one
    // timeline.
    let at = std::time::Instant::now();
    let plan = plan_of(exe, &["-n"]);
    let planned = at.elapsed();
    let n = count_in(&plan, "to restart (");
    let skipped = count_in(&plan, "skipped (");

    clear(&mut out);
    println!("\x1b[1mtaimux: restart every outdated session\x1b[0m\n");

    let Some(n) = n.filter(|n| *n >= 1) else {
        println!("Nothing to restart. Either every session is already on the");
        println!("installed version, or the ones behind it are busy.");
        if let Some(s) = skipped {
            println!("\n  {} left alone.", s);
        }
        println!();
        any_key();
        return;
    };

    for l in plan_panes(&plan) {
        println!("{}", l);
    }
    if let Some(s) = skipped {
        println!("\n  {} left alone (working, waiting, or unidentified).", s);
    }
    print!("\n\x1b[1mRestart {} session(s)?\x1b[0m [Y/n] ", n);
    let _ = out.flush();
    // An unreadable terminal declines, which is the safe default, but it SAYS
    // so: a silent "no" here is indistinguishable from the user answering n,
    // and the two want very different things done about them.
    let go = match read_one_key() {
        Pressed::Key(c) => restart::confirm_yes(&c.to_string()),
        Pressed::Unreadable => {
            print!("\r\n  (no readable terminal to answer on, so nothing was restarted)");
            false
        }
    };
    println!();
    if go {
        note(&format!(
            "sweep: {} to restart, plan took {:.1}s, {:.1}s from keypress to firing",
            n,
            planned.as_secs_f32(),
            at.elapsed().as_secs_f32()
        ));
        restart_detached(exe, "", false);
        println!(
            "\nStarted, detached. Watch the version column, or read\n{}",
            taimux_core::paths::runtime_dir()
                .join("restart.log")
                .display()
        );
        std::thread::sleep(std::time::Duration::from_millis(1200));
    } else {
        println!("\nNothing restarted.");
        std::thread::sleep(std::time::Duration::from_millis(700));
    }
}

/// A plan, as text, by asking ourselves for one.
///
/// A subprocess rather than a direct call, and that is not laziness: the plan has
/// to be the SAME text the reader would get from `taimux restart -n`, and the
/// only way to be sure of that is to run it.
fn plan_of(exe: &str, args: &[&str]) -> String {
    std::process::Command::new(exe)
        .arg("restart")
        .args(args)
        .output()
        .map(|o| {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            s
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {

    /// A detached job must not hold the terminal it was detached from.
    ///
    /// This is the F8 blank-popup bug, and it is invisible by inspection:
    /// `Command::spawn` inherits every stream it is not told about, so the
    /// omission looks like nothing at all. Asserted through /proc rather than
    /// by reading the source, because the whole defect was that the source
    /// looked fine.
    #[test]
    fn a_detached_job_does_not_hold_the_terminal() {
        let dir = std::env::temp_dir().join(format!("taimux-detached-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let logp = dir.join("log");
        let out = std::fs::File::create(&logp).unwrap();
        let err = out.try_clone().unwrap();

        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("30");
        // Hand it a terminal FIRST, so `detached` has something hostile to
        // override. Without this the test is vacuous wherever the harness's own
        // stdin is already /dev/null, which is most of CI: inheriting would give
        // /dev/null too and the assertion would pass on the broken code.
        let had_tty = match std::fs::File::open("/dev/tty") {
            Ok(tty) => {
                cmd.stdin(std::process::Stdio::from(tty));
                true
            }
            Err(_) => false,
        };
        let mut child = detached(&mut cmd, out, err).spawn().expect("spawn sleep");

        let fd0 = std::fs::read_link(format!("/proc/{}/fd/0", child.id()));
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);

        // /proc is Linux-only, and so is everything else here that reads it.
        let Ok(fd0) = fd0 else { return };
        if !had_tty {
            // No controlling terminal to be wrongly kept, so there is nothing
            // here to prove. Said out loud rather than passing quietly.
            eprintln!("no /dev/tty in this environment, so this proves nothing");
            return;
        }
        let fd0 = fd0.to_string_lossy().into_owned();
        assert!(
            !fd0.contains("/pts/") && !fd0.contains("/dev/tty"),
            "a detached job kept a terminal on stdin: {fd0}"
        );
        assert!(
            fd0.contains("null"),
            "expected /dev/null on stdin, got {fd0}"
        );
    }

    use super::*;

    const PLAN: &str = "claude: 2.1.258 installed at /l/claude\n\
                        \n\
                        to restart (2):\n\
                        \x20 %19   platform:4.1   a title\n\
                        \x20       2.1.100 -> 2.1.258, pane map, idle\n\
                        \x20       command claude --resume /t.jsonl\n\
                        \x20 %23   platform:4.5   another\n\
                        \x20       2.1.100 -> 2.1.258, pane map, idle\n\
                        \x20       command claude\n\
                        \n\
                        skipped (3):\n\
                        \x20 %77 main:1.1  2.1.100, run: rerun when idle\n";

    /// The pane lines only: the detail lines are indented further and the skip
    /// list sits past its own heading, so a guess at indentation would take
    /// either of them.
    #[test]
    fn only_the_pane_lines_of_the_plan_are_shown() {
        assert_eq!(
            plan_panes(PLAN),
            vec![
                "  %19   platform:4.1   a title",
                "  %23   platform:4.5   another"
            ]
        );
    }

    #[test]
    fn the_counts_come_off_the_headings() {
        assert_eq!(count_in(PLAN, "to restart ("), Some(2));
        assert_eq!(count_in(PLAN, "skipped ("), Some(3));
        assert_eq!(count_in("nothing to restart.\n", "to restart ("), None);
    }

    #[test]
    fn a_panes_own_reason_is_picked_out_of_the_skip_list() {
        assert_eq!(
            why_for(PLAN, "%77").as_deref(),
            Some("%77 main:1.1  2.1.100, run: rerun when idle")
        );
        assert_eq!(why_for(PLAN, "%99"), None);
    }

    /// A plan with nothing in it must not be read as having something: that is
    /// the difference between firing a restart and explaining a refusal.
    #[test]
    fn an_empty_plan_is_not_mistaken_for_a_full_one() {
        let empty = "claude: 2.1.258 installed at /l\n\nnothing to restart.\n";
        assert!(plan_panes(empty).is_empty());
        assert_eq!(count_in(empty, "to restart ("), None);
    }
}
