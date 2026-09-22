//! Which tmux panes are running a coding agent.
//!
//! A port of the bash prototype's `agent_of()` and the join in `_agent_rows`, kept
//! deliberately faithful: the prototype was the specification, its tests are the
//! ones that found these rules, and this had to agree with it row for row before
//! it could replace it.

use crate::proc::Foreground;
use std::collections::HashMap;
use std::process::Command;

/// The agent a foreground argv names, or None.
///
/// Headless, SDK and sub-agent claude invocations are excluded: they are never a
/// pane's foreground, and listing one would offer a jump to a throwaway session.
pub fn agent_of(argv: &str) -> Option<&'static str> {
    if argv.is_empty() {
        return None;
    }
    let word = |w: &str| -> bool {
        // the awk original is /(^|\/| )claude( |$)/: a whole word, possibly at the
        // end of a path
        argv.split([' ', '/']).any(|t| t == w)
    };
    // claude self-updates by writing a whole new binary under
    // .../claude/versions/<version> and repointing the launcher, so a session
    // started from one of those files directly has no "claude" word in its argv
    // at all: the version IS the filename.
    let versioned = argv.contains("/claude/versions/");
    if (word("claude") || versioned)
        && !argv.contains("--output-format")
        && !argv.split(' ').any(|t| t == "-p" || t == "--print")
    {
        return Some("claude");
    }
    for name in ["codex", "opencode", "agy", "pi"] {
        if word(name) {
            return Some(name);
        }
    }
    if argv.contains("gemini") {
        return Some("gemini");
    }
    if argv.contains("antigravity") {
        return Some("antigravity");
    }
    None
}

/// One row of `tmux list-panes -a`, before the agent join.
#[derive(Debug, Clone)]
pub struct Pane {
    pub tty: String,
    pub id: String,
    pub target: String,
    pub cwd: String,
    pub comm: String,
    pub title: String,
}

impl Pane {
    pub fn parse(line: &str) -> Option<Pane> {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 5 {
            return None;
        }
        Some(Pane {
            tty: f[0].trim_start_matches("/dev/").to_string(),
            id: f[1].to_string(),
            target: f[2].to_string(),
            cwd: f[3].to_string(),
            comm: f[4].to_string(),
            // Only the title may be empty, and it may also be absent: tmux drops
            // the trailing field when the title is unset.
            title: f.get(5).unwrap_or(&"").to_string(),
        })
    }
}

/// The agent row for a pane: `pane_id \t target \t cwd \t agent \t pid \t argv \t title`.
///
/// The two passes are load-bearing. A launcher can name the agent in its own
/// argv (`npm exec opencode`), so a process whose argv[0] IS the agent wins over
/// one that merely mentions it, and the pid decides where the version is read.
pub fn agent_row(pane: &Pane, fg: &[Foreground]) -> Option<String> {
    let mine: Vec<&Foreground> = fg.iter().filter(|p| p.tty == pane.tty).collect();

    let mut found: Option<(&'static str, i32, String)> = None;
    for exact in [true, false] {
        if found.is_some() {
            break;
        }
        for p in &mine {
            let Some(agent) = agent_of(&p.argv) else {
                continue;
            };
            if exact {
                let arg0 = p.argv.split(' ').next().unwrap_or("");
                let base = arg0.rsplit('/').next().unwrap_or(arg0);
                if base != agent {
                    continue;
                }
            }
            found = Some((agent, p.pid, p.argv.clone()));
            break;
        }
    }

    // Fallback only when NO foreground argv was seen for this tty, i.e. the scan
    // missed it. It must not override a deliberate exclusion: a headless claude
    // still has comm "claude".
    let (agent, pid, argv) = match found {
        Some(v) => v,
        None => {
            if mine.is_empty()
                && matches!(
                    pane.comm.as_str(),
                    "claude" | "codex" | "opencode" | "gemini" | "antigravity" | "agy" | "pi"
                )
            {
                (
                    match pane.comm.as_str() {
                        "claude" => "claude",
                        "codex" => "codex",
                        "opencode" => "opencode",
                        "gemini" => "gemini",
                        "antigravity" => "antigravity",
                        "agy" => "agy",
                        _ => "pi",
                    },
                    0,
                    "-".to_string(),
                )
            } else {
                return None;
            }
        }
    };

    let mut title = pane.title.clone();
    if agent == "agy" {
        title = title
            .trim_start_matches(['¯', '‾', '_'])
            .trim_start()
            .to_string();
        title = if title.is_empty() {
            "✳".to_string()
        } else {
            format!("✳ {}", title)
        };
    }
    let cwd = if pane.cwd.is_empty() { "?" } else { &pane.cwd };

    Some(format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}",
        pane.id, pane.target, cwd, agent, pid, argv, title
    ))
}

/// One `tmux list-panes -a`, in the same format the bash version asks for.
///
/// The only fork left on this path. tmux offers no other way in; control mode
/// would remove even this, which is the obvious next step.
pub fn tmux_panes() -> Vec<Pane> {
    let out = Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{pane_tty}\t#{pane_id}\t#{session_name}:#{window_index}.#{pane_index}\t#{pane_current_path}\t#{pane_current_command}\t#{pane_title}",
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(Pane::parse)
            .collect(),
        _ => Vec::new(),
    }
}

// ---- moved out of main.rs: composing the rows a pane scan turns into.
/// The full row the picker consumes:
///   pane_id \t target \t cwd \t agent \t version \t state \t mode \t title
///
/// Only the trailing title may be empty. Every earlier field carries a
/// placeholder instead, because bash's `read` collapses runs of tabs and a blank
/// field would silently shift every field after it.
pub fn list_rows(
    prober: &mut crate::version::Prober,
    captures: &mut HashMap<String, String>,
) -> String {
    let fg = crate::proc::foreground_map();
    let mut s = String::new();
    for pane in tmux_panes() {
        let Some(row) = agent_row(&pane, &fg) else {
            continue;
        };
        let f: Vec<&str> = row.split('\t').collect();
        let (id, target, cwd, agent) = (f[0], f[1], f[2], f[3]);
        let pid: i32 = f[4].parse().unwrap_or(0);
        let (argv, title) = (f[5], f[6]);

        let screen = match captures.get(id) {
            Some(c) => c.clone(),
            None => {
                let c = crate::tmux::capture(id).unwrap_or_default();
                captures.insert(id.to_string(), c.clone());
                c
            }
        };
        let hook = crate::hook::current(id, pid);
        let merged = crate::state::merge(&screen, hook.as_ref().map(|e| e.state.as_str()));
        let mode = hook.map(|e| e.mode).unwrap_or_else(|| "-".into());

        let exe = std::fs::read_link(format!("/proc/{}/exe", pid))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let exe = exe.trim_end_matches(" (deleted)"); // replaced by an update
        let ver = if pid == 0 {
            None
        } else {
            crate::version::from_path(exe, agent)
                .or_else(|| crate::version::from_script(argv))
                .or_else(|| prober.probe(exe, agent))
        };

        s.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            id,
            target,
            cwd,
            agent,
            ver.unwrap_or_default(),
            merged.as_str(),
            mode,
            title
        ));
    }
    s
}

/// What the session in one pane is doing, read exactly as the list reads it:
/// the hook line brought up to date by the transcript, then the screen. For the
/// callers that ask about a pane at a time, `print-cmds` and `resurrect`.
pub fn state_of(id: &str, pid: i32) -> crate::state::State {
    let screen = crate::tmux::capture(id).unwrap_or_default();
    let hook = crate::hook::current(id, pid);
    crate::state::merge(&screen, hook.as_ref().map(|e| e.state.as_str()))
}

pub fn agent_rows() -> String {
    let fg = crate::proc::foreground_map();
    let mut s = String::new();
    for pane in tmux_panes() {
        if let Some(row) = agent_row(&pane, &fg) {
            s.push_str(&row);
            s.push('\n');
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fg(tty: &str, pid: i32, argv: &str) -> Foreground {
        Foreground {
            tty: tty.into(),
            pid,
            argv: argv.into(),
        }
    }
    fn pane(tty: &str, id: &str, comm: &str, title: &str) -> Pane {
        Pane {
            tty: tty.into(),
            id: id.into(),
            target: "w:1.1".into(),
            cwd: "/w".into(),
            comm: comm.into(),
            title: title.into(),
        }
    }

    #[test]
    fn native_agents_are_matched_as_whole_words() {
        assert_eq!(agent_of("claude --effort max"), Some("claude"));
        assert_eq!(agent_of("/home/u/.local/bin/claude"), Some("claude"));
        assert_eq!(agent_of("codex"), Some("codex"));
        assert_eq!(agent_of("/home/u/.bun/bin/opencode run"), Some("opencode"));
    }

    #[test]
    fn a_node_wrapped_cli_is_matched_on_its_argv() {
        assert_eq!(
            agent_of("node /usr/lib/node_modules/@google/gemini-cli/dist/index.js"),
            Some("gemini")
        );
    }

    #[test]
    fn headless_and_sdk_claude_are_excluded() {
        assert_eq!(agent_of("claude -p hi --output-format stream-json"), None);
        assert_eq!(agent_of("claude --print hi"), None);
    }

    #[test]
    fn a_self_updated_claude_has_no_claude_word_at_all() {
        // the version IS the filename after a self-update
        assert_eq!(
            agent_of("/home/u/.local/share/claude/versions/2.1.239"),
            Some("claude")
        );
        assert_eq!(
            agent_of(
                "/home/u/.local/share/claude/versions/2.1.239 -p hi --output-format stream-json"
            ),
            None
        );
    }

    #[test]
    fn programs_that_merely_look_like_agents_are_not() {
        assert_eq!(agent_of("python -m pip install foo"), None); // not `pi`
        assert_eq!(agent_of("vim README.md"), None);
        assert_eq!(agent_of(""), None);
    }

    #[test]
    fn a_pane_joins_to_the_agent_on_its_own_tty() {
        let fgs = vec![
            fg("pts/10", 1010, "claude --effort max"),
            fg("pts/99", 9, "vim"),
        ];
        let row = agent_row(&pane("pts/10", "%10", "claude", "✳ A"), &fgs).unwrap();
        assert_eq!(
            row,
            "%10\tw:1.1\t/w\tclaude\t1010\tclaude --effort max\t✳ A"
        );
    }

    #[test]
    fn the_agent_wins_over_the_launcher_leading_its_group() {
        // npm names opencode in its own argv; the agent's own process must win,
        // because the pid decides where the version is read from
        let fgs = vec![
            fg("pts/14", 100, "npm exec opencode"),
            fg("pts/14", 101, "/home/u/.bun/bin/opencode run"),
        ];
        let row = agent_row(&pane("pts/14", "%14", "node", ""), &fgs).unwrap();
        assert!(
            row.contains("\t101\t"),
            "expected the agent's own pid, got {row}"
        );
    }

    #[test]
    fn comm_is_the_fallback_only_when_nothing_was_seen() {
        let row = agent_row(&pane("pts/19", "%19", "codex", "I"), &[]).unwrap();
        assert!(row.contains("\tcodex\t0\t-\t"), "{row}");
        // …and it must not resurrect a pane the argv rules deliberately excluded
        let fgs = vec![fg(
            "pts/11",
            1011,
            "claude -p hi --output-format stream-json",
        )];
        assert!(agent_row(&pane("pts/11", "%11", "claude", ""), &fgs).is_none());
    }

    #[test]
    fn a_pane_running_nothing_interesting_is_not_a_row() {
        let fgs = vec![fg("pts/18", 1018, "vim README.md")];
        assert!(agent_row(&pane("pts/18", "%18", "vim", "H"), &fgs).is_none());
    }

    #[test]
    fn agy_titles_are_normalised_the_way_the_picker_expects() {
        let fgs = vec![fg("pts/20", 1020, "agy --resume")];
        let row = agent_row(&pane("pts/20", "%20", "agy", "¯J"), &fgs).unwrap();
        assert!(row.ends_with("\t✳ J"), "{row}");
        let row = agent_row(&pane("pts/20", "%20", "agy", "_"), &fgs).unwrap();
        assert!(row.ends_with("\t✳"), "{row}");
    }

    #[test]
    fn tmux_drops_the_trailing_field_when_a_title_is_unset() {
        let p = Pane::parse("/dev/pts/7\t%7\tw:1.1\t/w\tclaude").unwrap();
        assert_eq!(p.title, "");
        assert_eq!(p.tty, "pts/7");
    }

    #[test]
    fn a_pane_with_no_cwd_is_marked_rather_than_left_blank() {
        // blank fields ahead of the title would collapse under bash's `read`
        let mut p = pane("pts/5", "%5", "claude", "t");
        p.cwd = String::new();
        let fgs = vec![fg("pts/5", 5, "claude")];
        assert!(agent_row(&p, &fgs).unwrap().contains("\t?\t"));
    }
}
