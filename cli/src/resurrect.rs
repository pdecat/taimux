//! Teaching tmux-resurrect which conversation each pane was on.
//!
//! tmux-resurrect saves a pane's command line and restores it verbatim, which for
//! a claude pane means a NEW session in the right directory: the conversation is
//! lost. This rewrites the save file so each pane comes back on the conversation
//! it was actually having.
//!
//! Every rule here exists because the failure mode is losing a saved layout, and
//! that is not recoverable:
//!
//! - **The join is of one moment.** The save file names a pane by
//!   session/window/pane INDEX, `print-cmds` names it by pane ID, and only tmux
//!   joins the two, so both are read while the panes are still live.
//! - **Only field 11 is ever touched**, so the line count cannot drop, and a
//!   rewrite that lost lines is refused rather than written.
//! - **The temporary file lives beside the target**, not in `/tmp`: across
//!   filesystems `mv` stops being a rename and stops being atomic. Two continuum
//!   saves DO overlap in practice, so a half-written save is a real outcome.
//! - **The symlink is followed**, not replaced: `last` points at the timestamped
//!   save and resurrect reads it through that link.
//! - **A pane with nothing to replay still comes back as claude**, in the right
//!   directory. An empty pane where a session used to be is silent; a claude
//!   prompt there says come and look at this one.

use std::collections::HashMap;
use std::path::Path;

/// A pane, as the save file names it: session, window index, pane index.
type Key = (String, String, String);

/// The pane records in a save file, split into fields.
///
/// Tab-separated, and the command is field 11 with a leading `:` marker that
/// resurrect puts there.
fn pane_lines(save: &str) -> Vec<Vec<&str>> {
    save.lines()
        .filter(|l| l.starts_with("pane\t"))
        .map(|l| l.split('\t').collect())
        .collect()
}

/// The command a save file holds for one pane, stripped back to a FRESH claude:
/// everything it was launched with except the flags that claim a conversation.
///
/// This is the fallback for a pane that read as claude but had no live session
/// process under it, so there is nothing to resume. Same three shapes dropped as
/// on a restart, and for the same reason.
pub fn fresh_from_saved(key: &Key, save: &str) -> String {
    let mut out = String::from("command claude");
    for f in pane_lines(save) {
        if f.len() < 11 {
            continue;
        }
        if (f[1].to_string(), f[2].to_string(), f[5].to_string()) != *key {
            continue;
        }
        // field 11 opens with the marker resurrect adds
        let cmd = f[10].strip_prefix(':').unwrap_or(f[10]);
        let words: Vec<&str> = cmd.split_whitespace().collect();
        // Skip the program, which is one word OR two: a save file that taimux
        // has already rewritten once opens with `command claude`.
        //
        // **This is a deliberate divergence from bash**, and the one place in
        // this port where the old behaviour was a bug rather than a decision. The
        // awk dropped `$1` only, so a field 11 reading `command claude --model
        // opus` came back as `command claude claude --model opus`: the restored
        // pane would start a fresh session prompted with the word "claude".
        // Reproduced against the bash function before it was deleted. It bites
        // exactly when a pane loses its session process AFTER a previous
        // resurrect pass has rewritten its command, which is not a rare pair.
        let mut i = 1;
        if words.first() == Some(&"command") && words.len() > 1 {
            i = 2;
        }
        while i < words.len() {
            match words[i] {
                "--fork-session" | "-c" | "--continue" => {}
                "--resume" | "-r" | "--session-id" => {
                    if i + 1 < words.len() && !words[i + 1].starts_with('-') {
                        i += 1;
                    }
                }
                w => {
                    out.push(' ');
                    out.push_str(w);
                }
            }
            i += 1;
        }
        break;
    }
    out
}

/// The save file with each mapped pane's command replaced.
///
/// Field 11 and nothing else, so the line count is unchanged by construction:
/// that is what makes the "did it lose lines?" check below meaningful.
pub fn rewrite(map: &HashMap<Key, String>, save: &str) -> String {
    let mut out = String::new();
    for line in save.lines() {
        if let Some(rest) = line.strip_prefix("pane\t") {
            let mut f: Vec<&str> = rest.split('\t').collect();
            // fields shift by one because the "pane" tag was stripped
            if f.len() >= 10 {
                let key = (f[0].to_string(), f[1].to_string(), f[4].to_string());
                if let Some(cmd) = map.get(&key) {
                    let replaced = format!(":{}", cmd);
                    f[9] = &replaced;
                    out.push_str("pane\t");
                    out.push_str(&f.join("\t"));
                    out.push('\n');
                    continue;
                }
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

pub struct Outcome {
    pub resumed: usize,
    pub fresh: usize,
    pub lost: usize,
    pub notes: Vec<String>,
    pub map: HashMap<Key, String>,
}

impl Outcome {
    pub fn total(&self) -> usize {
        self.resumed + self.fresh + self.lost
    }

    pub fn summary(&self) -> String {
        format!(
            "{} claude pane(s): {} resumed, {} fresh, {} left alone",
            self.total(),
            self.resumed,
            self.fresh,
            self.lost
        )
    }
}

/// Work out what each pane should come back as.
///
/// `panes` is `tmux list-panes -a -F '#{pane_id}\t#{session_name}\t#{window_index}\t#{pane_index}'`
/// and `records` is `print-cmds`. Both are read while the panes are live, which
/// is what makes the join of one moment.
pub fn decide(panes: &str, records: &str, save: &str) -> Outcome {
    let mut by_id: HashMap<&str, Key> = HashMap::new();
    for l in panes.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() >= 4 {
            by_id.insert(f[0], (f[1].into(), f[2].into(), f[3].into()));
        }
    }

    let mut o = Outcome {
        resumed: 0,
        fresh: 0,
        lost: 0,
        notes: Vec::new(),
        map: HashMap::new(),
    };
    for l in records.lines() {
        // Only the command may be empty, and it is last: `print-cmds` guarantees
        // the other five are populated, because an empty field anywhere else
        // would shift every value after it.
        let f: Vec<&str> = l.split('\t').collect();
        if f.is_empty() || f[0].is_empty() {
            continue;
        }
        let (pane, tgt) = (f[0], f.get(1).copied().unwrap_or(""));
        let status = f.get(3).copied().unwrap_or("");
        let why = f.get(4).copied().unwrap_or("");
        let cmd = f.get(5).copied().unwrap_or("");

        let Some(key) = by_id.get(pane) else {
            o.lost += 1;
            o.notes.push(format!(
                "{}: pane {} vanished between the two reads",
                tgt, pane
            ));
            continue;
        };
        let cmd = if cmd.is_empty() {
            o.fresh += 1;
            o.notes.push(format!(
                "{}: fresh session, {}, DIG",
                tgt,
                if why.is_empty() {
                    "no live session process"
                } else {
                    why
                }
            ));
            fresh_from_saved(key, save)
        } else if status == "resume" {
            o.resumed += 1;
            cmd.to_string()
        } else {
            o.fresh += 1;
            o.notes.push(format!("{}: fresh session, {}", tgt, why));
            cmd.to_string()
        };
        o.map.insert(key.clone(), cmd);
    }
    o
}

/// Write the rewritten save file over the target, atomically, refusing anything
/// that lost lines.
pub fn commit(target: &Path, body: &str, original: &str) -> Result<(), String> {
    if body.lines().count() < original.lines().count() {
        return Err("rewrite dropped lines, kept the original".into());
    }
    let dir = target.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!(".taimux-resurrect.{}.tmp", std::process::id()));
    std::fs::write(&tmp, body).map_err(|e| format!("rewrite failed: {}", e))?;
    // The saved file's own mode, so a rewrite does not change who can read it.
    if let Ok(m) = std::fs::metadata(target) {
        let _ = std::fs::set_permissions(&tmp, m.permissions());
    }
    std::fs::rename(&tmp, target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rewrite failed: {}", e)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A save file's pane record, in the eleven fields tmux-resurrect really
    /// writes. Taken from a live save file rather than guessed, because guessing
    /// put the command in field 10 and every assertion here then passed for the
    /// wrong reason:
    ///
    ///   pane  adm  1  1  :*Z  1  _  :/home/p/dir  0  bash  :
    fn pane(sess: &str, win: &str, idx: &str, cmd: &str) -> String {
        format!(
            "pane\t{}\t{}\t1\t:*Z\t{}\t_\t:/w\t0\tbash\t:{}\n",
            sess, win, idx, cmd
        )
    }

    fn key(s: &str, w: &str, p: &str) -> Key {
        (s.into(), w.into(), p.into())
    }

    #[test]
    fn only_the_command_field_changes() {
        let save = format!(
            "{}{}",
            pane("main", "1", "0", "claude"),
            "window\tmain\t1\n"
        );
        let mut map = HashMap::new();
        map.insert(
            key("main", "1", "0"),
            "command claude --resume /t.jsonl".into(),
        );
        let out = rewrite(&map, &save);
        assert!(out.contains(":command claude --resume /t.jsonl"));
        // every other field survives, and so does every other line
        assert!(out.contains(":*Z")); // the flags field, untouched
        assert!(out.contains("window\tmain\t1"));
        assert_eq!(out.lines().count(), save.lines().count());
    }

    /// A pane the map does not mention is left exactly as it was: a rewrite has
    /// no business touching a shell or an editor.
    #[test]
    fn an_unmapped_pane_is_untouched() {
        let save = pane("main", "1", "0", "vim");
        let out = rewrite(&HashMap::new(), &save);
        assert_eq!(out, save);
    }

    #[test]
    fn a_fresh_command_drops_every_claim_on_a_conversation() {
        let save = pane(
            "main",
            "1",
            "0",
            "claude --model opus --resume /old.jsonl --fork-session",
        );
        assert_eq!(
            fresh_from_saved(&key("main", "1", "0"), &save),
            "command claude --model opus"
        );
    }

    /// A save file taimux has already rewritten once opens with `command
    /// claude`, two words. bash dropped only the first, so this came back as
    /// `command claude claude --model opus` and the restored pane started a fresh
    /// session prompted with the word "claude". Reproduced against the bash
    /// function before deleting it; this is the one deliberate divergence in the
    /// port.
    #[test]
    fn a_command_prefixed_save_does_not_double_the_program() {
        let save = pane(
            "main",
            "1",
            "0",
            "command claude --model opus --resume /old.jsonl",
        );
        assert_eq!(
            fresh_from_saved(&key("main", "1", "0"), &save),
            "command claude --model opus"
        );
    }

    #[test]
    fn a_pane_the_save_does_not_hold_still_gives_a_bare_claude() {
        assert_eq!(
            fresh_from_saved(&key("nope", "9", "9"), &pane("main", "1", "0", "claude")),
            "command claude"
        );
    }

    #[test]
    fn a_resumable_record_is_counted_and_mapped() {
        let panes = "%1\tmain\t1\t0\n";
        let records =
            "%1\tmain:1.0\t/w\tresume\tpane map, idle\tcommand claude --resume /t.jsonl\n";
        let o = decide(panes, records, "");
        assert_eq!((o.resumed, o.fresh, o.lost), (1, 0, 0));
        assert_eq!(
            o.map.get(&key("main", "1", "0")).map(|s| s.as_str()),
            Some("command claude --resume /t.jsonl")
        );
        assert_eq!(
            o.summary(),
            "1 claude pane(s): 1 resumed, 0 fresh, 0 left alone"
        );
    }

    /// An unresolved record still comes back as claude, just without a
    /// conversation, and it says so in the notes.
    #[test]
    fn an_unresolved_record_comes_back_fresh_with_a_note() {
        let panes = "%1\tmain\t1\t0\n";
        let records = "%1\tmain:1.0\t/w\tunresolved\t3 share this title, idle\tcommand claude\n";
        let o = decide(panes, records, "");
        assert_eq!((o.resumed, o.fresh, o.lost), (0, 1, 0));
        assert!(o.notes[0].contains("fresh session, 3 share this title"));
    }

    /// A record with no command at all had no live session process under it, and
    /// the save file is where its argv comes from instead. Marked DIG, because it
    /// is the case worth looking at.
    #[test]
    fn a_record_with_no_command_falls_back_to_the_save_file() {
        let panes = "%1\tmain\t1\t0\n";
        let records = "%1\tmain:1.0\t/w\tunresolved\tno session process\t\n";
        let save = pane("main", "1", "0", "claude --resume /old.jsonl");
        let o = decide(panes, records, &save);
        assert_eq!((o.resumed, o.fresh, o.lost), (0, 1, 0));
        assert_eq!(
            o.map.get(&key("main", "1", "0")).map(|s| s.as_str()),
            Some("command claude")
        );
        assert!(o.notes[0].contains("DIG"));
    }

    /// The two reads are of one moment, but not the SAME instant: a pane can go
    /// between them, and that is counted rather than guessed at.
    #[test]
    fn a_pane_that_vanished_between_the_reads_is_left_alone() {
        let records = "%9\tmain:9.9\t/w\tresume\tpane map, idle\tcommand claude\n";
        let o = decide("%1\tmain\t1\t0\n", records, "");
        assert_eq!((o.resumed, o.fresh, o.lost), (0, 0, 1));
        assert!(o.notes[0].contains("vanished between the two reads"));
        assert!(o.map.is_empty());
    }

    /// Losing the saved layout is not recoverable, so a rewrite that dropped
    /// lines is refused. Only field 11 is ever touched, so it cannot happen, and
    /// that is exactly why the check is cheap enough to keep.
    #[test]
    fn a_rewrite_that_lost_lines_is_refused() {
        let d = std::env::temp_dir().join(format!("jmrr{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("last");
        std::fs::write(&f, "a\nb\nc\n").unwrap();
        assert_eq!(
            commit(&f, "a\nb\n", "a\nb\nc\n"),
            Err("rewrite dropped lines, kept the original".into())
        );
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "a\nb\nc\n");
        assert!(commit(&f, "a\nb\nc\n", "a\nb\nc\n").is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The temporary file goes beside the target, or `mv` stops being atomic
    /// across filesystems. Checked by there being nothing left behind.
    #[test]
    fn nothing_is_left_behind_beside_the_target() {
        let d = std::env::temp_dir().join(format!("jmrr2{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("last");
        std::fs::write(&f, "x\n").unwrap();
        commit(&f, "y\n", "x\n").expect("committed");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "y\n");
        let left: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, vec!["last".to_string()]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
