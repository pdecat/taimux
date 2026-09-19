//! Carrying a conversation into a different agent.
//!
//! The list beside this one answers "where did that session get to". This
//! answers the question that usually comes next: **continue it somewhere else**.
//! A Codex session you want Claude to finish; a Claude session whose directory
//! you want Gemini to look at; a Gemini conversation that cannot be resumed at
//! all, which is the case that makes this more than a convenience.
//!
//! The receiving agent is started with one prompt already typed, carrying four
//! things, in the order a model reads them:
//!
//! 1. **The instruction**, first, because that is what a model acts on.
//! 2. **The task**, which is what the session was called or opened with.
//! 3. **The git state**, which is the ground truth of what was actually done,
//!    and the half a conversation is worst at reporting honestly.
//! 4. **The last turns**, yours and its, oldest first.
//!
//! Then a pointer to the transcript, so the model can read the whole history
//! itself rather than being limited to what fitted. That pointer is the reason
//! the turn budget can stay small: the prompt is an orientation, not an archive.
//!
//! Ported from rses, whose shape this is, with one change: the turns come from
//! taimux's own readers, so the caps are in characters rather than bytes and a
//! multi-byte character cannot be cut in half.

use crate::agents;

/// How much of each part survives into the prompt.
///
/// The last thing the agent said gets more room than the rest, because it is the
/// work that was in flight and the thing most likely to be half-finished.
const TASK_MAX: usize = 800;
const TURN_MAX: usize = 600;
const LAST_ANSWER_MAX: usize = 1200;

/// How many turns are carried. Six is what rses settled on and what the preview
/// already shows, so the prompt and the pane agree about what "recently" means.
pub const DEFAULT_TURNS: usize = 6;

fn trunc(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    format!("{}…", s.chars().take(max).collect::<String>())
}

/// The proper name of an agent, for prose rather than for a command line.
pub fn display_name(agent: &str) -> &str {
    match agent {
        "claude" => "Claude Code",
        "codex" => "Codex",
        "gemini" => "Gemini",
        "opencode" => "OpenCode",
        "agy" => "Antigravity",
        other => other,
    }
}

/// The handoff prompt for one conversation.
pub fn build(agent: &str, key: &str, turns: usize) -> String {
    let meta = agents::meta(agent, key);
    let name = display_name(agent);
    let mut out = String::new();

    let article = if name.starts_with(['A', 'E', 'I', 'O', 'U']) {
        "an"
    } else {
        "a"
    };
    out.push_str(&format!(
        "Continue this work. You are picking up from {} {} session.\n",
        article, name
    ));
    if !meta.cwd.is_empty() {
        out.push_str(&format!("Work in: {}\n", meta.cwd));
    }
    let git = git_context(&meta.cwd);
    if let Some(b) = &git.branch {
        out.push_str(&format!("Branch: {}\n", b));
    }

    let task = trunc(&meta.title, TASK_MAX);
    out.push_str("\nTask:\n  ");
    out.push_str(if task.is_empty() {
        "(not found)"
    } else {
        &task
    });
    out.push('\n');

    if let Some(log) = &git.log {
        out.push_str("\nRecent commits:\n");
        for l in log.lines() {
            out.push_str(&format!("  {}\n", l));
        }
    }
    if let Some(status) = &git.status {
        out.push_str("\nUncommitted changes:\n");
        for l in status.lines() {
            out.push_str(&format!("  {}\n", l));
        }
    }

    let said = agents::turns(agent, key, turns);
    if !said.is_empty() {
        out.push_str(&format!(
            "\nRecent conversation ({} messages):\n",
            said.len()
        ));
        let last = said.len() - 1;
        for (i, t) in said.iter().enumerate() {
            let label = if t.you { "User" } else { name };
            let max = if !t.you && i == last {
                LAST_ANSWER_MAX
            } else {
                TURN_MAX
            };
            out.push_str(&format!("  {}: {}\n", label, trunc(&t.text, max)));
        }
    }

    if let Some(path) = agents::transcript(agent, key) {
        out.push_str(&format!("\nFull session transcript: {}\n", path));
        out.push_str("Read this file if you need the complete conversation history.\n");
    }

    out
}

/// The command that starts an agent with a prompt already typed.
///
/// Every one of them takes it differently, and two of them would otherwise run
/// it non-interactively, which is the opposite of what a handoff is for:
///
/// - `claude` and `codex` take a bare first argument;
/// - `opencode --prompt` opens its interface with the prompt in, where
///   `opencode run` would answer once and exit;
/// - `agy -i` is `--prompt-interactive`, and plain `--prompt` is its print mode;
/// - `gemini` takes a bare argument too.
///
/// **Through `command`**, like every other line taimux builds, so no shell alias
/// fires and tmux-resurrect can read the argv back later.
pub fn launch(target: &str, prompt: &str) -> Result<String, String> {
    let q = crate::tmux::shell_quote(prompt);
    Ok(match target {
        "claude" | "codex" | "gemini" => format!("command {} {}", target, q),
        "opencode" => format!("command opencode --prompt {}", q),
        "agy" => format!("command agy -i {}", q),
        other => {
            return Err(format!(
                "taimux does not know how to start {} with a prompt",
                other
            ))
        }
    })
}

/// Which agents are on this machine, in the order they are offered.
///
/// Asked of `PATH` rather than assumed, because the list is a menu: offering a
/// target that is not installed spends a keystroke on an error, and the whole
/// point of the menu is that it is one keystroke.
pub fn installed() -> Vec<&'static str> {
    crate::agents::KNOWN
        .iter()
        .copied()
        .filter(|a| on_path(a))
        .collect()
}

fn on_path(exe: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    path.split(':').any(|d| {
        let p = std::path::Path::new(d).join(exe);
        p.is_file()
            && std::os::unix::fs::PermissionsExt::mode(&match p.metadata() {
                Ok(m) => m.permissions(),
                Err(_) => return false,
            }) & 0o111
                != 0
    })
}

/// What a repository says about itself, or nothing at all.
#[derive(Debug, Default, PartialEq)]
pub struct Git {
    pub branch: Option<String>,
    pub log: Option<String>,
    pub status: Option<String>,
}

/// The branch, the recent commits and the dirty files of the directory a session
/// ran in.
///
/// Every part is optional and a directory that is not a repository yields all
/// three as None: plenty of sessions are not in one, and a handoff from those is
/// the conversation alone.
pub fn git_context(cwd: &str) -> Git {
    if cwd.is_empty() || !std::path::Path::new(cwd).is_dir() {
        return Git::default();
    }
    let run = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
        (!s.is_empty()).then_some(s)
    };
    // The cheap question first: anything that is not a repository fails here and
    // costs one fork rather than three.
    let Some(branch) = run(&["rev-parse", "--abbrev-ref", "HEAD"]) else {
        return Git::default();
    };
    Git {
        log: run(&["log", "--oneline", "-10"]),
        status: run(&["status", "--porcelain", "--untracked-files=no"]),
        branch: Some(branch),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_gets_the_article_it_needs() {
        assert_eq!(display_name("agy"), "Antigravity");
        assert!(build("nosuchagent", "k", 3).contains("a nosuchagent session"));
        // Antigravity takes "an", which is the only reason the rule is here
        assert!(build("agy", "/nowhere", 3).contains("an Antigravity session"));
    }

    #[test]
    fn a_cut_lands_on_a_character_boundary() {
        let s = "é".repeat(50);
        let t = trunc(&s, 10);
        assert_eq!(t.chars().count(), 11); // ten plus the ellipsis
        assert!(t.ends_with('…'));
        assert_eq!(trunc("  short  ", 10), "short");
    }

    /// A directory that is not a repository contributes nothing, rather than
    /// three empty headings.
    #[test]
    fn somewhere_that_is_not_a_repository_says_nothing_about_git() {
        assert_eq!(git_context("/nowhere/at/all"), Git::default());
        assert_eq!(git_context(""), Git::default());
        let tmp = std::env::temp_dir().join(format!("tmxgit{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        assert_eq!(git_context(&tmp.to_string_lossy()), Git::default());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The prompt opens with the instruction, because that is what a model acts
    /// on, and the task comes before the evidence.
    #[test]
    fn the_prompt_leads_with_the_instruction() {
        let p = build("claude", "/nowhere/at/all.jsonl", 3);
        assert!(p.starts_with("Continue this work."), "{p}");
        assert!(p.contains("Task:"), "{p}");
    }
}
