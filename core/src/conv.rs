//! Which conversation a claude pane is on.
//!
//! Comes first because the indexer needs it: nothing can be indexed until it is
//! known which transcript each live pane is talking to.
//!
//! Two rungs here, in this order, and the order is the whole design:
//!
//! 1. **The pane map**, written by a `SessionStart` hook, so it covers startup,
//!    resume, clear and compact alike. Ranked top because it is rewritten on
//!    every one of those, which means it tracks a `/clear` or an in-session
//!    `/resume` that leaves the argv below stale.
//! 2. **The live argv**, which covers every session a previous restart pass
//!    relaunched.
//!
//! The rest of the ladder (title matching, the newest-transcript fallback) lands
//! with restart, which is the only thing that needs it.

use std::path::{Path, PathBuf};

use crate::hook::json_str;

/// Where claude keeps its state.
pub fn claude_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".claude"))
}

/// A working directory, in the shape claude names its project folders: every
/// character that is not a letter or a digit becomes a dash.
pub fn project_dir_for(cwd: &str) -> PathBuf {
    let slug: String = cwd
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    claude_dir().join("projects").join(slug)
}

/// A process's argv, NUL-separated in `/proc`, as a vector.
pub fn argv_of(pid: i32) -> Vec<String> {
    let Ok(raw) = std::fs::read(format!("/proc/{}/cmdline", pid)) else {
        return Vec::new();
    };
    raw.split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// The transcript a session id names, when exactly one file starts with it.
///
/// Ambiguity is a refusal rather than a guess: a prefix matching two
/// conversations names neither.
pub fn transcript_by_id(id: &str) -> Option<PathBuf> {
    if id.is_empty() || id.starts_with('-') || id.contains('/') {
        return None;
    }
    let root = claude_dir().join("projects");
    let mut hit = None;
    // maxdepth 2 from the projects dir: one level of project directory, then the
    // files in it.
    for proj in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        for f in std::fs::read_dir(proj.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let name = f.file_name().to_string_lossy().into_owned();
            if name.starts_with(id) && name.ends_with(".jsonl") {
                if hit.is_some() {
                    return None; // more than one: names neither
                }
                hit = Some(f.path());
            }
        }
    }
    hit
}

/// What the resolver found, and how, so a refusal can say why.
#[derive(Debug, PartialEq, Eq)]
pub struct Resolved {
    pub transcript: PathBuf,
    pub why: &'static str,
}

/// The argv rungs, pulled out so they can be tested without a live process.
///
/// A **forked** session is the trap: it runs as
/// `--session-id <new> --fork-session --resume <PARENT transcript>`, writing a
/// brand-new transcript named by `--session-id` while the `--resume` value names
/// the parent, whose own pane is somewhere else entirely. So when
/// `--fork-session` is present the `--resume` value is dropped and the id wins.
///
/// `claude attach <session-id>` names a session outright, and only as the FIRST
/// argument: a flag in front of it (Patrick's shell alias once put `--effort max`
/// there) shifts it out of the subcommand slot and claude reads the whole thing
/// as a prompt, so a later `attach` is a word somebody typed, not a session.
fn from_argv(argv: &[String]) -> (Option<String>, Option<String>, Option<String>) {
    let mut forked = false;
    let (mut sid, mut res, mut att) = (None, None, None);
    let val = |i: usize| -> Option<String> {
        argv.get(i + 1)
            .filter(|v| !v.starts_with('-'))
            .map(|v| v.to_string())
    };
    // Indexed on purpose: every one of these flags is read together with the
    // argument AFTER it, so an iterator over the values alone cannot do it.
    #[allow(clippy::needless_range_loop)]
    for i in 1..argv.len() {
        match argv[i].as_str() {
            "--fork-session" => forked = true,
            "--session-id" => sid = val(i).or(sid),
            "-r" | "--resume" => res = val(i).or(res),
            _ => {}
        }
    }
    if argv.len() > 2 && argv[1] == "attach" && !argv[2].starts_with('-') {
        att = Some(argv[2].clone());
    }
    if forked {
        res = None;
    }
    (att, sid, res)
}

/// The transcript this pane's session is writing to, or None.
///
/// `cwd` is claude's own working directory, and the pane map is only trusted
/// when it agrees: that check is what stops a nested `claude -p` (which inherits
/// `$TMUX_PANE` from the session that launched it) handing over its throwaway
/// conversation.
pub fn resolve_from_pane(pane: &str, cwd: &str, pid: i32) -> Option<Resolved> {
    let mf = claude_dir()
        .join("tmux-panes")
        .join(format!("{}.json", pane.trim_start_matches('%')));
    if let Ok(text) = std::fs::read_to_string(&mf) {
        let mpath = json_str(&text, "transcript_path").unwrap_or_default();
        let mcwd = json_str(&text, "cwd").unwrap_or_default();
        if !mpath.is_empty() && Path::new(&mpath).is_file() && mcwd == cwd {
            return Some(Resolved {
                transcript: PathBuf::from(mpath),
                why: "pane map",
            });
        }
    }

    let pdir = project_dir_for(cwd);
    if !pdir.is_dir() {
        return None;
    }
    let argv = argv_of(pid);
    if argv.is_empty() {
        return None;
    }
    let (att, sid, res) = from_argv(&argv);

    // Looked up before the two below because it is the most explicit of the
    // three, and separately from them because it is the one shape that can put a
    // pane on a conversation belonging to ANOTHER directory: attach runs wherever
    // you happen to be, while the session keeps the project it was created in.
    if let Some(a) = att {
        if let Some(t) = transcript_by_id(&a) {
            return Some(Resolved {
                transcript: t,
                why: "attach in argv",
            });
        }
    }
    for (v, why) in [(sid, "--session-id in argv"), (res, "--resume in argv")] {
        let Some(v) = v else { continue };
        let p = if v.starts_with('/') {
            PathBuf::from(&v)
        } else {
            pdir.join(format!("{}.jsonl", v))
        };
        if p.is_file() {
            return Some(Resolved { transcript: p, why });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_project_dir_dashes_everything_that_is_not_alphanumeric() {
        let _guard = crate::env::ENV_LOCK.lock().unwrap();
        std::env::set_var("CLAUDE_CONFIG_DIR", "/c");
        assert_eq!(
            project_dir_for("/home/p/work/my_proj.v2"),
            PathBuf::from("/c/projects/-home-p-work-my-proj-v2")
        );
        std::env::remove_var("CLAUDE_CONFIG_DIR");
    }

    #[test]
    fn resume_and_session_id_are_read_out_of_the_argv() {
        let (att, sid, res) = from_argv(&v(&["claude", "--resume", "abc"]));
        assert_eq!((att, sid, res), (None, None, Some("abc".into())));
        let (_, sid, _) = from_argv(&v(&["claude", "--session-id", "def"]));
        assert_eq!(sid, Some("def".into()));
        // a flag where a value should be is not a value
        let (_, _, res) = from_argv(&v(&["claude", "--resume", "--verbose"]));
        assert_eq!(res, None);
    }

    /// A forked session writes a NEW transcript named by --session-id while
    /// --resume names its parent, whose pane is somewhere else entirely. Taking
    /// the resume value would put two panes on one conversation.
    #[test]
    fn a_forked_session_drops_the_resume_value() {
        let (_, sid, res) = from_argv(&v(&[
            "claude",
            "--session-id",
            "new",
            "--fork-session",
            "--resume",
            "/parent.jsonl",
        ]));
        assert_eq!(sid, Some("new".into()));
        assert_eq!(res, None);
    }

    /// `attach` counts only in the subcommand slot. Patrick's shell alias once
    /// put `--effort max` in front of it, which shifted it out and made claude
    /// read the whole line as a prompt.
    #[test]
    fn attach_counts_only_as_the_first_argument() {
        let (att, ..) = from_argv(&v(&["claude", "attach", "263946b5"]));
        assert_eq!(att, Some("263946b5".into()));
        let (att, ..) = from_argv(&v(&["claude", "--effort", "max", "attach", "263946b5"]));
        assert_eq!(att, None);
        // …and a flag is not a session id
        let (att, ..) = from_argv(&v(&["claude", "attach", "--help"]));
        assert_eq!(att, None);
    }

    #[test]
    fn argv_of_reads_this_process() {
        let me = argv_of(std::process::id() as i32);
        assert!(!me.is_empty());
        // …and a pid nothing is running under is empty rather than an error
        assert!(argv_of(0).is_empty());
    }

    /// An ambiguous prefix names neither conversation, which is a refusal rather
    /// than a coin toss.
    #[test]
    fn an_ambiguous_session_id_resolves_to_nothing() {
        let _guard = crate::env::ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("jmconv{}", std::process::id()));
        let proj = root.join("projects/-p");
        std::fs::create_dir_all(&proj).unwrap();
        std::env::set_var("CLAUDE_CONFIG_DIR", &root);

        std::fs::write(proj.join("abc111.jsonl"), "").unwrap();
        assert_eq!(transcript_by_id("abc111"), Some(proj.join("abc111.jsonl")));
        assert_eq!(transcript_by_id("abc"), Some(proj.join("abc111.jsonl")));

        std::fs::write(proj.join("abc222.jsonl"), "").unwrap();
        assert_eq!(transcript_by_id("abc"), None); // now ambiguous
        assert_eq!(transcript_by_id("abc111"), Some(proj.join("abc111.jsonl")));

        // nothing shaped like a path or a flag is looked up at all
        assert_eq!(transcript_by_id("a/b"), None);
        assert_eq!(transcript_by_id("-r"), None);
        assert_eq!(transcript_by_id(""), None);

        std::env::remove_var("CLAUDE_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The pane map wins, but only when its cwd agrees: a nested `claude -p`
    /// inherits $TMUX_PANE and would otherwise hand over its throwaway
    /// conversation.
    #[test]
    fn the_pane_map_is_trusted_only_where_the_cwd_agrees() {
        let _guard = crate::env::ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("jmpane{}", std::process::id()));
        let panes = root.join("tmux-panes");
        std::fs::create_dir_all(&panes).unwrap();
        std::env::set_var("CLAUDE_CONFIG_DIR", &root);
        let tr = root.join("t.jsonl");
        std::fs::write(&tr, "").unwrap();
        std::fs::write(
            panes.join("7.json"),
            format!(r#"{{"transcript_path":"{}","cwd":"/work"}}"#, tr.display()),
        )
        .unwrap();

        let got = resolve_from_pane("%7", "/work", 0).expect("resolved");
        assert_eq!(got.why, "pane map");
        assert_eq!(got.transcript, tr);

        // a different cwd: not this pane's conversation
        assert_eq!(resolve_from_pane("%7", "/elsewhere", 0), None);

        std::env::remove_var("CLAUDE_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod attach_tests {
    use super::*;

    /// `attach` runs wherever you happen to be, while the session keeps the
    /// project it was created in. So this is the one rung that can put a pane on
    /// a conversation belonging to ANOTHER directory, and it has to be looked up
    /// by id rather than under the pane's own project.
    ///
    /// Live example: `platform:17.1` sat in `$HOME` with its conversation under
    /// `configurations/generic`, and it was the only pane the indexer failed to
    /// resolve when the pid was being read out of the wrong column.
    #[test]
    fn attach_reaches_a_conversation_in_another_project() {
        let _guard = crate::env::ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("jmatt{}", std::process::id()));
        let here = root.join("projects/-home-p"); // where the pane is
        let there = root.join("projects/-w-other"); // where the session lives
        std::fs::create_dir_all(&here).unwrap();
        std::fs::create_dir_all(&there).unwrap();
        std::env::set_var("CLAUDE_CONFIG_DIR", &root);
        let tr = there.join("263946b5-9bd7-493d-8be1-10b0c180dd34.jsonl");
        std::fs::write(&tr, "").unwrap();

        // A real pid is needed for the argv rung, so this exercises from_argv
        // plus transcript_by_id, which is the whole of that rung's logic.
        let argv: Vec<String> = ["claude", "attach", "263946b5"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (att, ..) = from_argv(&argv);
        assert_eq!(att.as_deref(), Some("263946b5"));
        assert_eq!(transcript_by_id(&att.unwrap()), Some(tr));

        std::env::remove_var("CLAUDE_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// How far back the title scan looks, and how many transcripts it will consider.
///
/// Both are bounds on a project directory that has accumulated: a title match
/// over hundreds of conversations is slow AND more likely to collide, and a
/// month-old transcript is not what a live pane is talking to.
const SCAN_CAP: usize = 12;
const SCAN_MAX_AGE_DAYS: u64 = 30;

/// The titles a transcript recorded, newest first, deduplicated.
///
/// Only the last eight title records are read: they interleave for the whole
/// life of a session, so the newest few are the ones a pane could be showing, and
/// reading the whole file would cost a title match its affordability.
pub fn titles_of(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut records = 0;
    for line in text.lines().rev() {
        if !(line.contains("\"type\":\"custom-title\"") || line.contains("\"type\":\"ai-title\"")) {
            continue;
        }
        records += 1;
        if records > 8 {
            break;
        }
        let t = {
            let c = crate::json::field(line, "customTitle");
            if c.is_empty() {
                crate::json::field(line, "aiTitle")
            } else {
                c
            }
        };
        if !t.is_empty() && seen.insert(t.clone()) {
            out.push(t);
        }
    }
    out
}

/// A title with its leading punctuation and trailing space taken off, so a pane
/// title and a recorded one can be compared at all.
pub fn norm_title(t: &str) -> String {
    t.trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim_end()
        .to_string()
}

/// The recent transcripts in a project, newest first.
pub fn recent_in(pdir: &Path) -> Vec<PathBuf> {
    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(SCAN_MAX_AGE_DAYS * 86400));
    let mut v: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for e in std::fs::read_dir(pdir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(m) = std::fs::metadata(&p) else {
            continue;
        };
        let Ok(t) = m.modified() else { continue };
        if let Some(c) = cutoff {
            if t < c {
                continue;
            }
        }
        v.push((t, p));
    }
    v.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    v.truncate(SCAN_CAP);
    v.into_iter().map(|(_, p)| p).collect()
}

/// Does one title identify the other? Both directions, because a pane title can
/// be truncated OR carry an appended fork marker.
fn title_matches(pane_norm: &str, variants: &[String], recorded: &str) -> bool {
    if recorded.chars().count() < 8 {
        return false;
    }
    // A fork is a separate session titled "<parent> ⑂ <spawning prompt>", and it
    // inherits the parent's title, so it otherwise prefix-matches the parent's
    // pane. If THIS pane carries no fork marker, a candidate that does is
    // somebody else.
    if !pane_norm.contains('⑂') && recorded.contains('⑂') {
        return false;
    }
    variants.iter().any(|v| {
        v.chars().count() >= 8 && (v.starts_with(recorded) || recorded.starts_with(v.as_str()))
    })
}

/// The pane-title variants worth testing: the title as it stands, and without the
/// hook's "<project>: " prefix, since a transcript may only ever have recorded the
/// unprefixed form.
fn title_variants(pane_norm: &str) -> Vec<String> {
    let mut v = vec![pane_norm.to_string()];
    if let Some((_, rest)) = pane_norm.split_once(": ") {
        v.push(rest.to_string());
    }
    v
}

/// The session ids a pane's own scrollback mentions.
///
/// Claude spills large tool output to `projects/<proj>/<session-id>/tool-results/`
/// and prints that path, so an id on THIS screen is pane-anchored evidence. Only
/// ever used to NARROW an existing title match, never on its own: a session that
/// discusses other sessions (any agent-tooling work, this one included) has their
/// ids on screen too.
fn ids_on_screen(pane: &str) -> Vec<String> {
    let Some(screen) = crate::tmux::ask_raw(&["capture-pane", "-p", "-t", pane, "-S", "-2000"])
    else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for (i, _) in screen.match_indices("/tool-results") {
        // a uuid is 36 characters, immediately before the marker
        if i < 36 {
            continue;
        }
        let cand = &screen[i - 36..i];
        if is_uuid(cand) && !out.contains(&cand.to_string()) {
            out.push(cand.to_string());
        }
    }
    out
}

fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    b.iter().enumerate().all(|(i, c)| match i {
        8 | 13 | 18 | 23 => *c == b'-',
        _ => c.is_ascii_hexdigit(),
    })
}

/// The whole ladder: the pane map, the argv, the pane title, and a project with
/// only one recent transcript in it.
///
/// `Err` carries why it refused, which is the whole point of a refusal here:
/// restarting the wrong conversation is worse than not restarting.
pub fn resolve(pane: &str, cwd: &str, title: &str, pid: i32) -> Result<Resolved, String> {
    if let Some(r) = resolve_from_pane(pane, cwd, pid) {
        return Ok(r);
    }
    let pdir = project_dir_for(cwd);
    if !pdir.is_dir() {
        return Err(format!("no project dir for {}", cwd));
    }
    let files = recent_in(&pdir);
    let pn = norm_title(title);
    let variants = title_variants(&pn);

    if pn.chars().count() >= 8 && !files.is_empty() {
        let mut hits: Vec<PathBuf> = Vec::new();
        for f in &files {
            for t in titles_of(f) {
                if title_matches(&pn, &variants, &norm_title(&t)) {
                    hits.push(f.clone());
                    break;
                }
            }
        }
        match hits.len() {
            1 => {
                return Ok(Resolved {
                    transcript: hits.remove(0),
                    why: "title match",
                })
            }
            0 => {}
            _ => {
                // Recurring task titles (six sessions named "Check ZHA mesh
                // status") cannot be told apart by title. The pane's scrollback
                // can.
                let onscreen = ids_on_screen(pane);
                if !onscreen.is_empty() {
                    let narrowed: Vec<&PathBuf> = hits
                        .iter()
                        .filter(|h| {
                            h.file_stem()
                                .map(|s| onscreen.iter().any(|o| o == &s.to_string_lossy()))
                                .unwrap_or(false)
                        })
                        .collect();
                    if narrowed.len() == 1 {
                        return Ok(Resolved {
                            transcript: narrowed[0].clone(),
                            why: "title match narrowed by a tool-results path on screen",
                        });
                    }
                }
                return Err(format!("{} transcripts share this title", hits.len()));
            }
        }
    }

    if files.len() == 1 {
        return Ok(Resolved {
            transcript: files[0].clone(),
            why: "only transcript in project",
        });
    }
    Err(format!(
        "no pane map, and no title match among {} recent transcripts",
        files.len()
    ))
}

/// The command line that puts this pane's session back.
///
/// Everything the live argv carries is replayed EXCEPT the three shapes that
/// would answer the same question twice or answer it wrongly:
///
/// - `-c` / `--continue` is a guess wearing the clothes of a resume;
/// - `--fork-session` names the PARENT transcript rather than this pane's;
/// - `--session-id`, `-r` and `--resume` go WITH their values, since the resolved
///   transcript already decides which session this is and a stale explicit id
///   would either clash or claim an id another process holds;
/// - `attach <id>` in the subcommand slot is the same claim in subcommand form.
///
/// `command claude`, so no shell alias fires and tmux-resurrect can read the argv
/// back later.
pub fn build_cmd(argv: &[String], transcript: &str, ccwd: &str, panepath: &str) -> Option<String> {
    if argv.is_empty() {
        return None;
    }
    let mut keep: Vec<&str> = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "-c" | "--continue" | "--fork-session" => {}
            "attach" => {
                let claims = i == 1 && i + 1 < argv.len() && !argv[i + 1].starts_with('-');
                if claims {
                    i += 1;
                } else {
                    keep.push(&argv[i]);
                }
            }
            "--session-id" | "-r" | "--resume" => {
                if i + 1 < argv.len() && !argv[i + 1].starts_with('-') {
                    i += 1;
                }
            }
            other => keep.push(other),
        }
        i += 1;
    }
    let mut cmd = String::from("command claude");
    for a in keep {
        cmd.push(' ');
        cmd.push_str(&crate::tmux::shell_quote(a));
    }
    if !transcript.is_empty() {
        cmd.push_str(&format!(
            " --resume {}",
            crate::tmux::shell_quote(transcript)
        ));
    }
    // Only when claude's own cwd is not where the pane shell will land.
    if ccwd != panepath {
        cmd = format!("cd {} && {}", crate::tmux::shell_quote(ccwd), cmd);
    }
    Some(cmd)
}

#[cfg(test)]
mod ladder_tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_title_loses_its_leading_glyph_and_trailing_space() {
        assert_eq!(norm_title("✳ proj: do the thing  "), "proj: do the thing");
        assert_eq!(norm_title("plain"), "plain");
        assert_eq!(norm_title("  ⑂ forked  "), "forked");
    }

    /// Both directions, because a pane title can be truncated OR carry an
    /// appended fork marker.
    #[test]
    fn a_title_matches_in_either_direction() {
        let pn = "proj: do the thing";
        let vs = title_variants(pn);
        assert!(title_matches(pn, &vs, "proj: do the thing"));
        assert!(title_matches(pn, &vs, "proj: do the")); // recorded is a prefix
        assert!(title_matches(pn, &vs, "proj: do the thing and more")); // pane truncated
        assert!(!title_matches(pn, &vs, "other: thing"));
    }

    /// The unprefixed variant exists because a transcript may only ever have
    /// recorded the title without the hook's "<project>: " prefix.
    #[test]
    fn the_unprefixed_variant_is_tried_too() {
        let pn = "proj: do the thing";
        assert_eq!(
            title_variants(pn),
            vec!["proj: do the thing", "do the thing"]
        );
        assert!(title_matches(pn, &title_variants(pn), "do the thing"));
    }

    /// Anything under eight characters is not evidence: too many sessions share
    /// a short title for a prefix test to mean anything.
    #[test]
    fn a_short_title_is_never_a_match() {
        assert!(!title_matches("short", &title_variants("short"), "short"));
        assert!(!title_matches(
            "a long enough one",
            &title_variants("a long enough one"),
            "tiny"
        ));
    }

    /// A fork inherits its parent's title, so it prefix-matches the parent's
    /// pane. Without this the two are indistinguishable and the WRONG one gets
    /// restarted.
    #[test]
    fn a_fork_is_not_mistaken_for_its_parent() {
        let parent = "proj: the original task";
        assert!(!title_matches(
            parent,
            &title_variants(parent),
            "proj: the original task ⑂ and then this"
        ));
        // …but a forked PANE may match a forked title
        let fork = "proj: the original task ⑂ and then this";
        assert!(title_matches(
            fork,
            &title_variants(fork),
            "proj: the original task ⑂ and then this"
        ));
    }

    #[test]
    fn a_uuid_is_recognised_and_nothing_else_is() {
        assert!(is_uuid("263946b5-9bd7-493d-8be1-10b0c180dd34"));
        assert!(!is_uuid("263946b5-9bd7-493d-8be1-10b0c180dd3"));
        assert!(!is_uuid("263946b5x9bd7-493d-8be1-10b0c180dd34"));
        assert!(!is_uuid("zz3946b5-9bd7-493d-8be1-10b0c180dd34"));
    }

    /// Three shapes are dropped because each would answer the same question
    /// twice, and --session-id / --resume go with their VALUES.
    #[test]
    fn the_replay_drops_every_competing_claim() {
        let cmd = build_cmd(
            &v(&[
                "claude",
                "--continue",
                "--model",
                "opus",
                "--resume",
                "/old.jsonl",
            ]),
            "/new.jsonl",
            "/w",
            "/w",
        )
        .expect("cmd");
        assert_eq!(cmd, "command claude --model opus --resume /new.jsonl");

        let cmd = build_cmd(
            &v(&[
                "claude",
                "--session-id",
                "abc",
                "--fork-session",
                "-r",
                "/p.jsonl",
            ]),
            "/new.jsonl",
            "/w",
            "/w",
        )
        .expect("cmd");
        assert_eq!(cmd, "command claude --resume /new.jsonl");
    }

    #[test]
    fn the_replay_drops_an_attach_subcommand_with_its_id() {
        let cmd = build_cmd(
            &v(&["claude", "attach", "263946b5"]),
            "/t.jsonl",
            "/w",
            "/w",
        )
        .expect("cmd");
        assert_eq!(cmd, "command claude --resume /t.jsonl");
        // …but the word elsewhere is an ordinary argument
        let cmd = build_cmd(&v(&["claude", "--print", "attach"]), "", "/w", "/w").expect("cmd");
        assert_eq!(cmd, "command claude --print attach");
    }

    /// A cd is prepended only when claude's own directory is not where the pane
    /// shell will land, since the pane's own path is where a new shell starts.
    #[test]
    fn a_cd_is_added_only_when_the_directories_differ() {
        let same = build_cmd(&v(&["claude"]), "", "/w", "/w").expect("cmd");
        assert_eq!(same, "command claude");
        let diff = build_cmd(&v(&["claude"]), "", "/w/sub", "/w").expect("cmd");
        assert_eq!(diff, "cd /w/sub && command claude");
    }

    #[test]
    fn an_argument_needing_quotes_gets_them() {
        let cmd = build_cmd(&v(&["claude", "--prompt", "two words"]), "", "/w", "/w").expect("cmd");
        assert_eq!(cmd, "command claude --prompt 'two words'");
    }

    #[test]
    fn no_argv_is_no_command() {
        assert!(build_cmd(&[], "/t.jsonl", "/w", "/w").is_none());
    }
}

/// One record per claude pane: where it is, whether its conversation could be
/// identified, why, and the command that would put it back.
///
///   pane \t target \t cwd \t resume|unresolved \t <why>, <state> \t <command>
///
/// This is what `resurrect` rewrites a save file from and what `restart` decides
/// from, so the format is load-bearing and the equivalence bar for it is byte
/// identity across every live pane.
pub fn print_cmds(rows: &str, state_of: &dyn Fn(&str, i32) -> String) -> String {
    let mut out = String::new();
    // Resuming one conversation into two panes is as wrong on a restore as it is
    // on a restart, so the second pane to claim a transcript gets a fresh session.
    let mut claimed: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for line in rows.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 7 || f[3] != "claude" {
            continue;
        }
        let (id, target, cwd, title) = (f[0], f[1], f[2], f[6]);
        let pid: i32 = f[4].parse().unwrap_or(0);

        // A pane whose agent was detected only by the comm fallback carries no
        // pid, so there is no argv to replay and no cwd to read. It still gets a
        // record: the caller wants to know a claude lived here.
        if pid == 0 {
            out.push_str(&format!(
                "{}\t{}\t{}\tunresolved\t{}\t\n",
                id,
                target,
                cwd,
                "looks like claude but no session process was found under the pane"
            ));
            continue;
        }

        let state = state_of(id, pid);
        let ccwd = std::fs::read_link(format!("/proc/{}/cwd", pid))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ccwd = if ccwd.is_empty() {
            cwd.to_string()
        } else {
            ccwd
        };

        let (mut transcript, mut why, mut status) = match resolve(id, &ccwd, title, pid) {
            Ok(r) => (
                r.transcript.to_string_lossy().into_owned(),
                r.why.to_string(),
                "resume",
            ),
            Err(w) => (String::new(), w, "unresolved"),
        };
        if !transcript.is_empty() {
            if let Some(first) = claimed.get(&transcript) {
                why = format!("resolves to the same transcript as {}", first);
                transcript.clear();
                status = "unresolved";
            } else {
                claimed.insert(transcript.clone(), id.to_string());
            }
        }

        let cmd = match build_cmd(&argv_of(pid), &transcript, &ccwd, cwd) {
            Some(c) => c,
            None => {
                status = "unresolved";
                why = "could not read its argv".into();
                String::new()
            }
        };
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}, {}\t{}\n",
            id, target, ccwd, status, why, state, cmd
        ));
    }
    out
}

#[cfg(test)]
mod resolve_tests {
    use super::*;

    /// A fixture claude directory: two conversations in one project, with titles.
    ///
    /// These cases came from the bash suite, which tested the ladder by stubbing
    /// `_argv_of`. A compiled resolver reads `/proc`, so the argv rungs are
    /// covered separately (see `from_argv` above) and this covers the rungs that
    /// read the filesystem: the pane map with its cwd check, the title match and
    /// its two refusals, and the only-transcript fallback.
    struct Fix {
        root: PathBuf,
        pdir: PathBuf,
    }

    impl Fix {
        fn new(tag: &str) -> Fix {
            let root = std::env::temp_dir().join(format!("jmres{}{}", std::process::id(), tag));
            let _ = std::fs::remove_dir_all(&root);
            let pdir = root.join("projects/-w");
            std::fs::create_dir_all(&pdir).unwrap();
            std::fs::create_dir_all(root.join("tmux-panes")).unwrap();
            std::env::set_var("CLAUDE_CONFIG_DIR", &root);
            Fix { root, pdir }
        }

        fn transcript(&self, name: &str, title: &str) -> PathBuf {
            let p = self.pdir.join(format!("{}.jsonl", name));
            std::fs::write(
                &p,
                format!(
                    "{{\"type\":\"user\"}}\n{{\"type\":\"custom-title\",\"customTitle\":\"{}\"}}\n",
                    title
                ),
            )
            .unwrap();
            p
        }

        fn pane_map(&self, pane: &str, transcript: &Path, cwd: &str) {
            std::fs::write(
                self.root.join(format!("tmux-panes/{}.json", pane)),
                format!(
                    r#"{{"transcript_path":"{}","cwd":"{}"}}"#,
                    transcript.display(),
                    cwd
                ),
            )
            .unwrap();
        }
    }

    impl Drop for Fix {
        fn drop(&mut self) {
            std::env::remove_var("CLAUDE_CONFIG_DIR");
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn titles_are_read_newest_first_both_kinds_deduplicated() {
        let _g = crate::env::ENV_LOCK.lock().unwrap();
        let f = Fix::new("t");
        let p = f.pdir.join("t.jsonl");
        std::fs::write(
            &p,
            concat!(
                r#"{"type":"custom-title","customTitle":"the old one"}"#,
                "\n",
                r#"{"type":"ai-title","aiTitle":"merge the tools"}"#,
                "\n",
                r#"{"type":"custom-title","customTitle":"taimux: merge the tools"}"#,
                "\n",
                r#"{"type":"ai-title","aiTitle":"merge the tools"}"#,
                "\n"
            ),
        )
        .unwrap();
        // newest first, both kinds, and the repeat appears once
        assert_eq!(
            titles_of(&p),
            vec!["merge the tools", "taimux: merge the tools", "the old one"]
        );
    }

    /// A nested `claude -p` inherits $TMUX_PANE and overwrites the pane's map
    /// entry with its own throwaway session. The cwd equality check catches it,
    /// and resolution then falls through to the title.
    #[test]
    fn the_pane_map_is_trusted_only_for_its_own_cwd() {
        let _g = crate::env::ENV_LOCK.lock().unwrap();
        let f = Fix::new("m");
        let a = f.transcript("aaaa", "proj: the first task");
        let b = f.transcript("bbbb", "proj: the second task");

        f.pane_map("7", &a, "/w");
        let r = resolve("%7", "/w", "proj: the second task", 0).expect("resolved");
        assert_eq!((r.transcript.as_path(), r.why), (a.as_path(), "pane map"));

        // the same map entry, but written by something running elsewhere
        f.pane_map("7", &a, "/elsewhere");
        let r = resolve("%7", "/w", "proj: the second task", 0).expect("resolved");
        assert_eq!(
            (r.transcript.as_path(), r.why),
            (b.as_path(), "title match")
        );
    }

    #[test]
    fn a_unique_title_resolves_and_a_shared_one_refuses() {
        let _g = crate::env::ENV_LOCK.lock().unwrap();
        let f = Fix::new("s");
        let a = f.transcript("aaaa", "proj: the first task");
        f.transcript("bbbb", "proj: the second task");

        let r = resolve("%9", "/w", "proj: the first task", 0).expect("resolved");
        assert_eq!(r.transcript, a);
        assert_eq!(r.why, "title match");

        // two conversations recording the same title cannot be told apart
        f.transcript("cccc", "proj: the first task");
        let e = resolve("%9", "/w", "proj: the first task", 0).expect_err("refused");
        assert_eq!(e, "2 transcripts share this title");
    }

    /// A fork inherits its parent's title, so it prefix-matches the parent's
    /// pane. Without the marker rule the pair is ambiguous and the wrong one wins.
    #[test]
    fn a_forked_candidate_is_excluded_for_an_unforked_pane() {
        let _g = crate::env::ENV_LOCK.lock().unwrap();
        let f = Fix::new("f");
        let a = f.transcript("aaaa", "proj: the first task");
        f.transcript("bbbb", "proj: the first task ⑂ and then this");
        let r = resolve("%9", "/w", "proj: the first task", 0).expect("resolved");
        assert_eq!(r.transcript, a);
    }

    /// A project with exactly one recent transcript leaves nothing to confuse it
    /// with, so a pane with no usable title still resolves.
    #[test]
    fn a_project_with_one_transcript_needs_no_title() {
        let _g = crate::env::ENV_LOCK.lock().unwrap();
        let f = Fix::new("o");
        let a = f.transcript("aaaa", "x");
        let r = resolve("%9", "/w", "short", 0).expect("resolved");
        assert_eq!(r.transcript, a);
        assert_eq!(r.why, "only transcript in project");
    }

    #[test]
    fn an_unknown_cwd_names_the_missing_project_dir() {
        let _g = crate::env::ENV_LOCK.lock().unwrap();
        let _f = Fix::new("u");
        let e = resolve("%9", "/no/such/place", "a title long enough", 0).expect_err("refused");
        assert!(e.contains("no project dir"), "{}", e);
    }

    /// Neither a short pane title nor an empty project resolves, and the refusal
    /// counts what it looked at.
    #[test]
    fn no_title_and_several_candidates_refuses_with_a_count() {
        let _g = crate::env::ENV_LOCK.lock().unwrap();
        let f = Fix::new("n");
        f.transcript("aaaa", "one title here");
        f.transcript("bbbb", "another title");
        let e = resolve("%9", "/w", "tiny", 0).expect_err("refused");
        assert_eq!(
            e,
            "no pane map, and no title match among 2 recent transcripts"
        );
    }
}
