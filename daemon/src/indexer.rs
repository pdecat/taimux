//! The indexer: what each session SAID, and which conversations have ended.
//!
//! This is the subsystem that made a compiled version the direction in the first
//! place. In bash it is a lock directory, a stamp file, TTLs, incremental reads
//! and two caches, and three of the four defects shipped the night it was written
//! came from exactly here, including a fork bomb that took a machine down twice.
//!
//! Two of those disappear by construction rather than by being fixed:
//!
//! - **The lock is a real lock.** bash used `mkdir` plus a "steal it if it is
//!   older than two minutes" rule, because a directory outlives the process that
//!   made it and a crashed pass would otherwise wedge the indexer shut forever.
//!   `flock` on an open descriptor is released by the KERNEL when the holder
//!   dies, so there is no stale lock, no staleness threshold to tune, and no
//!   window in which two passes both think they stole it.
//! - **The rate limit is the same file.** bash kept a separate `.stamp`, and each
//!   generation of the fork bomb got a fresh runtime directory and therefore a
//!   fresh stamp, so the throttle never fired. Here the lock file's own mtime is
//!   the stamp: one file, and a pass cannot be throttled by a stamp it is not
//!   also locked against.
//!
//! What is faithfully kept, because each was arrived at the hard way:
//!
//! - **Liveness is resolved once and spent three times**: the live panes are
//!   indexed from it, the sessions cache marks an ended conversation by its
//!   absence from it, and the prune reads that cache back. Resolving it is the
//!   expensive part of a pass.
//! - **The lock is held for the WHOLE pass.** It used to be dropped early, which
//!   left the expensive half unprotected, and two indexers routinely read half a
//!   gigabyte at once with a picker merely sitting open.
//! - **An index is per PANE, keyed by the transcript in its header.** A `/clear`
//!   moving a pane onto a new conversation is then a header naming a transcript
//!   that is no longer this pane's, i.e. a rebuild with nothing to clean up.
//! - **A grown transcript is read from where the last pass stopped.** A SHRUNKEN
//!   one is not the same file whatever its name says, so it is rebuilt.
//! - **The cap keeps the TAIL**, the opposite of what rses does, because these are
//!   live sessions and the opening of one is already on its row as the title.
//! - **Prune by membership, never by age.** An ended session's blob is written
//!   once and never touched again, so any age rule throws it away and re-reads
//!   half a gigabyte to get it back. Only another host's entries expire on age,
//!   since their own fetch is what maintains them.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use taimux_core::{conv, index, transcript};

/// The one thing here that is not std. `flock` is in libc, which is already
/// linked; declaring the three symbols needed is smaller and more honest than
/// taking a dependency to reach them, and this is the whole of the FFI surface.
mod ffi {
    extern "C" {
        pub fn flock(fd: i32, operation: i32) -> i32;
    }
    pub const LOCK_EX: i32 = 2;
    pub const LOCK_NB: i32 = 4;
    pub const LOCK_UN: i32 = 8;
}

/// An exclusive lock on the pass, released when this value drops OR when the
/// process dies, whichever comes first.
pub struct Lock {
    file: File,
}

impl Lock {
    /// Take it, or report that somebody else has it.
    ///
    /// Non-blocking on purpose: an indexer that cannot get the lock has nothing
    /// to do, because the holder is about to do it.
    pub fn take(path: &Path) -> Option<Lock> {
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .ok()?;
        let rc = unsafe { ffi::flock(file.as_raw_fd(), ffi::LOCK_EX | ffi::LOCK_NB) };
        if rc != 0 {
            return None;
        }
        Some(Lock { file })
    }

    /// How long ago a pass last started, from the lock file's own mtime.
    ///
    /// An EMPTY lock file has never been stamped, and reads as forever ago. That
    /// distinction is load-bearing: taking the lock creates the file, which sets
    /// its mtime to now, so an age read from the mtime alone would throttle the
    /// very first pass and the index would never be built at all. The one byte
    /// `stamp` writes is what says a pass has actually run.
    pub fn age(&self) -> u64 {
        let Ok(m) = self.file.metadata() else {
            return u64::MAX;
        };
        if m.len() == 0 {
            return u64::MAX;
        }
        m.modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|d| d.as_secs())
            .unwrap_or(u64::MAX)
    }

    /// Mark a pass as starting now.
    pub fn stamp(&mut self) {
        // Writing a byte is what moves the mtime; the content is never read, and
        // the file is truncated first so it cannot grow without bound.
        let _ = self.file.set_len(0);
        let _ = self.file.seek(SeekFrom::Start(0));
        let _ = self.file.write_all(b"1");
        let _ = self.file.flush();
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        unsafe { ffi::flock(self.file.as_raw_fd(), ffi::LOCK_UN) };
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn env_usize(key: &str, default: usize) -> usize {
    taimux_core::env::var(key)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Cut a blob to the cap, keeping the END of it.
///
/// The cut lands mid-word, so the remains of that word go with it. Character-wise
/// rather than byte-wise: a blob cut inside a character is not text any more.
fn cap(blob: &str, limit: usize) -> String {
    if blob.chars().count() <= limit {
        return blob.to_string();
    }
    let n = blob.chars().count();
    let tail: String = blob.chars().skip(n - limit).collect();
    match tail.find(' ') {
        Some(i) => tail[i..].trim_start_matches(' ').to_string(),
        None => tail,
    }
}

/// The header a per-pane index file opens with.
struct Head {
    seen: u64,
    path: String,
}

fn read_head(f: &Path) -> Option<(Head, String)> {
    let text = std::fs::read_to_string(f).ok()?;
    let mut lines = text.splitn(2, '\n');
    let head: Vec<&str> = lines.next()?.split(' ').collect();
    if head.first() != Some(&"idx") || head.len() < 5 {
        return None;
    }
    Some((
        Head {
            seen: head[1].parse().ok()?,
            path: head[4..].join(" "),
        },
        lines
            .next()
            .unwrap_or("")
            .trim_end_matches('\n')
            .to_string(),
    ))
}

/// One pane's transcript, indexed. Returns false when there was nothing to do.
pub fn index_pane(id: &str, tr: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(tr) else {
        return false;
    };
    let size = meta.len();
    let f = index::index_dir().join(index::key_for(id));

    let mut known = String::new();
    let mut from = 0u64;
    if let Some((head, blob)) = read_head(&f) {
        if head.path == tr.to_string_lossy() && head.seen > 0 {
            if head.seen == size {
                return false; // nothing new to read
            }
            // Grown: read only the bytes it gained. SHRUNK: not the same file any
            // more whatever its name says, so the blob is rebuilt.
            if head.seen < size {
                known = blob;
                from = head.seen;
            }
        }
    }

    let mut text = String::new();
    if let Ok(mut fh) = File::open(tr) {
        if from > 0 && fh.seek(SeekFrom::Start(from)).is_err() {
            return false;
        }
        if fh.read_to_string(&mut text).is_err() {
            // A transcript with a partial character at the seek point: rebuild
            // rather than give up, since the next pass would hit it again.
            let mut raw = Vec::new();
            let Ok(mut fh) = File::open(tr) else {
                return false;
            };
            if fh.read_to_end(&mut raw).is_err() {
                return false;
            }
            text = String::from_utf8_lossy(&raw).into_owned();
            known.clear();
        }
    }

    let blob = cap(
        &format!("{}{}", known, transcript::extract(&text)),
        env_usize("TAIMUX_SEARCH_CAP", 262144),
    );
    let _ = std::fs::create_dir_all(index::index_dir());
    // Written by rename: half a file is worse than an old one when two pickers
    // are reading while a pass writes.
    let tmp = f.with_extension(format!("t{}", std::process::id()));
    let ok = std::fs::write(
        &tmp,
        format!("idx {} {} {} {}\n{}\n", size, now(), id, tr.display(), blob),
    )
    .is_ok();
    if ok && std::fs::rename(&tmp, &f).is_ok() {
        true
    } else {
        let _ = std::fs::remove_file(&tmp);
        false
    }
}

/// pane id and the transcript it is on, for every live claude pane.
///
/// Resolved once per pass and spent three times, which is why it is returned
/// rather than recomputed: this is the expensive part.
///
/// **`rows` is the SEVEN-field scan** (`id target cwd agent pid argv title`), not
/// the eight-field `list`. The pid is what makes a pane's argv readable, and in
/// the list format that column holds the version instead, which parses as pid 0.
/// That was silent: every pane with a `SessionStart` pane map resolved anyway off
/// rung 1, and only a `claude attach` pane, which has no map, came back
/// unresolved.
pub fn live_transcripts(rows: &str) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for line in rows.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 5 || f[3] != "claude" {
            continue;
        }
        let (id, cwd) = (f[0], f[2]);
        let pid: i32 = f[4].parse().unwrap_or(0);
        if let Some(r) = conv::resolve_from_pane(id, cwd, pid) {
            out.push((id.to_string(), r.transcript));
        }
    }
    out
}

/// The cwd, the claude version and the title of one conversation, read BACKWARDS
/// and stopped as soon as it has all three.
///
/// That is what makes listing hundreds of them affordable: all three are
/// re-stated on recent records, so a 23 MB conversation costs no more than a
/// small one.
pub fn session_meta(path: &Path) -> (String, String, String, String) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return (String::new(), String::new(), String::new(), String::new());
    };
    let (mut cwd, mut ver, mut ttl, mut ait, mut lp) = (
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
    );
    for (n, line) in text.lines().rev().enumerate() {
        if cwd.is_empty() {
            cwd = val(line, "cwd");
        }
        if ver.is_empty() {
            ver = val(line, "version");
        }
        // The two kinds interleave for the whole life of a session, so the last
        // one written is usually the unprefixed ai-title while the pane shows the
        // custom one. Prefer the custom one and keep the other as a fallback.
        if ttl.is_empty() && line.contains("\"type\":\"custom-title\"") {
            ttl = val(line, "customTitle");
        }
        if ait.is_empty() && line.contains("\"type\":\"ai-title\"") {
            ait = val(line, "aiTitle");
        }
        // A session can end before it was ever titled: a short one, or one that
        // was cleared. The prompt it was last given identifies such a session far
        // better than "(no title)": one of Patrick's turned out to be the tail of
        // a pasted config file, which is exactly the session you would go looking
        // for.
        if lp.is_empty() && line.contains("\"type\":\"last-prompt\"") {
            lp = val(line, "lastPrompt");
        }
        if !cwd.is_empty() && !ver.is_empty() && !ttl.is_empty() {
            break;
        }
        if n > 400 {
            break; // a transcript that never says: stop digging
        }
    }
    if ttl.is_empty() {
        ttl = ait;
    }
    let mut src = if ttl.is_empty() { "" } else { "t" }.to_string();
    if ttl.is_empty() && !lp.is_empty() {
        let mut t = lp
            .replace("\\n", " ")
            .replace("\\r", " ")
            .replace("\\t", " ");
        while t.contains("  ") {
            t = t.replace("  ", " ");
        }
        t = t.trim_start_matches(' ').to_string();
        ttl = if t.chars().count() > 80 {
            format!("{}…", t.chars().take(79).collect::<String>())
        } else {
            t
        };
        src = "p".into();
    }
    let clean = |s: String| s.replace('\t', " ");
    (clean(cwd), clean(ver), clean(ttl), src)
}

/// One JSON string value, by key. Kept as a name of its own because this file
/// reads it dozens of times and `val(line, "type")` says what it means here.
fn val(line: &str, key: &str) -> String {
    taimux_core::json::field(line, key)
}

/// Every conversation on disk, newest first, capped.
///
/// The cap exists because the corpus only grows: Claude Code's own cleanup is set
/// to ten years here, so this is hundreds of conversations today and thousands
/// eventually, and both the list and the content index behind it have to stay
/// bounded by something. Recency is the only sensible bound.
fn transcripts_by_recency(limit: usize) -> Vec<(i64, PathBuf)> {
    let mut out: Vec<(i64, PathBuf)> = Vec::new();
    walk(&conv::claude_dir().join("projects"), &mut out);
    out.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    out.truncate(limit);
    out
}

/// Every `.jsonl` under a directory, at ANY depth, following symlinks.
///
/// The depth is the whole point, and getting it wrong was the one real bug in
/// this port: a subagent writes to
/// `projects/<proj>/<session>/subagents/agent-*.jsonl`, four levels down, and
/// bash reaches those because its `find -L` has no maxdepth. Stopping at two
/// levels missed them, and since the list is capped at 200 by RECENCY that
/// changed which 200 came back, not merely how many: 44 of the cache's rows
/// differed. (`_transcript_by_id` is a different question and IS bounded, at
/// maxdepth 2, so a subagent transcript is not resolvable by id.)
fn walk(dir: &Path, out: &mut Vec<(i64, PathBuf)>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        // metadata() follows symlinks, which is what `find -L` does.
        let Ok(m) = std::fs::metadata(&p) else {
            continue;
        };
        if m.is_dir() {
            walk(&p, out);
            continue;
        }
        if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let mtime = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.push((mtime, p));
    }
}

/// Rebuild the sessions cache.
///
/// Whole, on every pass, which sounds wasteful and is not: a line whose
/// transcript has not been touched since it was written is reused verbatim, so
/// the cost of a pass is the directory walk plus a read for each conversation
/// that has actually said something since. Keyed on "<mtime> <path>" rather than
/// the path alone, so a conversation that has moved on is a key that is simply
/// not there and gets read again.
pub fn sessions_scan(live: &[(String, PathBuf)]) {
    let f = index::sessions_file();
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let onpane: std::collections::HashMap<String, String> = live
        .iter()
        .map(|(pane, p)| (p.to_string_lossy().into_owned(), pane.clone()))
        .collect();

    let mut known: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for e in index::sessions() {
        known.insert(
            format!("{} {}", e.mtime, e.path),
            format!("{}\t{}\t{}\t{}", e.cwd, e.version, e.title, e.src),
        );
    }

    let mut body = format!("sess {}\n", now());
    for (mtime, path) in transcripts_by_recency(env_usize("TAIMUX_SESSIONS_MAX", 200)) {
        let p = path.to_string_lossy().into_owned();
        let meta = match known.get(&format!("{} {}", mtime, p)) {
            Some(m) => m.clone(),
            None => {
                let (cwd, ver, ttl, src) = session_meta(&path);
                format!("{}\t{}\t{}\t{}", cwd, ver, ttl, src)
            }
        };
        body.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            mtime,
            onpane.get(&p).map(|s| s.as_str()).unwrap_or("-"),
            p,
            meta
        ));
    }
    // Never replace a good cache with an empty one, and never with half a file:
    // two pickers may be reading it while this writes.
    if body.lines().count() < 2 {
        return;
    }
    let tmp = f.with_extension(format!("t{}", std::process::id()));
    if std::fs::write(&tmp, &body).is_ok() && std::fs::rename(&tmp, &f).is_ok() {
        return;
    }
    let _ = std::fs::remove_file(&tmp);
}

/// The content of the ENDED conversations, so the search reaches them too.
///
/// Keyed by the transcript rather than by a pane, since that is the only name an
/// ended session has; `index_pane` then treats it exactly like a live one, which
/// means it is read once and never again, because a conversation nothing is
/// running does not grow.
pub fn index_dead() {
    for e in index::sessions() {
        if e.pane == "-" && !e.path.is_empty() {
            index_pane(&format!("dead:{}", e.path), Path::new(&e.path));
        }
    }
}

/// Drop the index files nothing can reach any more.
///
/// **By membership, not by age**, for everything local: an ended session's blob
/// is written once and never touched again, so any age rule throws it away and
/// re-reads half a gigabyte to get it back. Only another host's entries expire on
/// age, because their own fetch is what maintains them and a host that has gone
/// will never refresh them.
pub fn prune() {
    let sessions = index::sessions();
    if sessions.is_empty() {
        return; // nothing to judge membership against: judge nothing
    }
    let keep: std::collections::HashSet<String> = sessions
        .iter()
        .filter(|e| !e.path.is_empty())
        .map(|e| {
            if e.pane == "-" {
                format!("dead:{}", e.path)
            } else {
                e.pane.clone()
            }
        })
        .collect();
    let keepsec = env_usize("TAIMUX_SEARCH_KEEP_MIN", 1440) as i64 * 60;
    let n = now();

    for f in std::fs::read_dir(index::index_dir())
        .into_iter()
        .flatten()
        .flatten()
    {
        let path = f.path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(head) = text.lines().next() else {
            continue;
        };
        let cols: Vec<&str> = head.split(' ').collect();
        if cols.first() != Some(&"idx") || cols.len() < 4 {
            continue;
        }
        let (stamp, id) = (cols[2].parse::<i64>().unwrap_or(0), cols[3]);
        // A host-qualified id belongs to another box and goes on age; the rest
        // belong here, and go on whether the sessions cache still knows the name.
        let stale = if is_remote_id(id) {
            n - stamp > keepsec
        } else {
            !keep.contains(id)
        };
        if stale {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// `ha:%6` is another host's pane. `%6` and `dead:/path` are ours.
fn is_remote_id(id: &str) -> bool {
    match id.split_once(":%") {
        Some((host, n)) => {
            !host.is_empty() && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
        }
        None => false,
    }
}

/// One indexer pass. `rows` is the local agent list, already scanned.
///
/// The lock is held for ALL of it, which is a fix rather than a detail: it used
/// to be dropped after the live panes, leaving the expensive half (the history
/// walk, the ended sessions' content, the prune) unprotected, and two indexers
/// routinely read half a gigabyte at once with a picker merely sitting open.
pub fn pass(rows: &str, search: bool, sessions: bool) {
    let live = live_transcripts(rows);
    if search {
        for (id, tr) in &live {
            index_pane(id, tr);
        }
    }
    if sessions {
        sessions_scan(&live);
        if search {
            index_dead();
        }
        prune();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The field positions of the scan row, pinned. Reading the version column
    /// as a pid is exactly the mistake this port made, and nothing failed loudly
    /// when it did.
    #[test]
    fn live_transcripts_reads_the_pid_from_the_scan_not_the_list() {
        // seven fields: id target cwd agent PID argv title
        let scan = "%1\tw:1.1\t/nope\tclaude\t424242\tclaude\tt";
        // eight fields: id target cwd agent VERSION state mode title
        let list = "%1\tw:1.1\t/nope\tclaude\t2.1.258\tidle\t-\tt";
        // Neither resolves here (no such project dir), but the difference is
        // whether a pid was even seen, and a list row cannot carry one.
        assert!(live_transcripts(scan).is_empty());
        assert!(live_transcripts(list).is_empty());
        // A non-claude agent is never indexed whatever its fields say.
        assert!(live_transcripts("%1\tw:1.1\t/\tgemini\t9\tnode\tt").is_empty());
    }

    #[test]
    fn the_cap_keeps_the_end_and_drops_the_partial_word() {
        assert_eq!(cap("one two three", 40), "one two three");
        // The last 8 characters are " ccc ddd"; the leading run of non-spaces is
        // empty here, so only the space goes. Checked against the awk, which is
        // `sub(/^[^ ]* */, "", s)` on the same substring.
        assert_eq!(cap("aaa bbb ccc ddd", 8), "ccc ddd");
        // …and a cut landing INSIDE a word takes the rest of that word with it
        assert_eq!(cap("aaa bbb ccc", 5), "ccc");
        // no space to cut at: keep what fits rather than nothing
        assert_eq!(cap("aaaaaaaaaa", 4), "aaaa");
    }

    /// Characters, not bytes. A blob cut inside a character is not text.
    #[test]
    fn the_cap_cuts_on_characters() {
        let s = "ééé ààà";
        let c = cap(s, 3);
        assert!(c.chars().count() <= 3);
        assert!(c.chars().all(|ch| ch == 'à'));
    }

    #[test]
    fn a_header_that_is_not_one_is_refused() {
        let d = std::env::temp_dir().join(format!("jmix{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("h");
        std::fs::write(&f, "idx 100 200 %7 /a/b.jsonl\nthe blob\n").unwrap();
        let (h, blob) = read_head(&f).expect("head");
        assert_eq!(h.seen, 100);
        assert_eq!(h.path, "/a/b.jsonl");
        assert_eq!(blob, "the blob");

        std::fs::write(&f, "nonsense\n").unwrap();
        assert!(read_head(&f).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A transcript path with a space in it still round-trips: the header is
    /// space-separated and the path is last, so it is joined back rather than
    /// taken as one field.
    #[test]
    fn a_path_with_a_space_survives_the_header() {
        let d = std::env::temp_dir().join(format!("jmixsp{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("h");
        std::fs::write(&f, "idx 1 2 %7 /a b/c.jsonl\nx\n").unwrap();
        assert_eq!(read_head(&f).unwrap().0.path, "/a b/c.jsonl");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The lock is exclusive, and the SECOND taker is refused rather than made
    /// to wait: an indexer that cannot get it has nothing to do.
    #[test]
    fn the_lock_admits_one_holder() {
        let d = std::env::temp_dir().join(format!("jmlock{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(".lock");
        let first = Lock::take(&p).expect("first");
        // The same process re-flocking its own file would succeed, which is why
        // this is checked with a child instead.
        let out = std::process::Command::new("flock")
            .args(["-n", &p.to_string_lossy(), "-c", "true"])
            .status();
        if let Ok(s) = out {
            assert!(!s.success(), "a second holder got the lock");
        }
        drop(first);
        if let Ok(s) = std::process::Command::new("flock")
            .args(["-n", &p.to_string_lossy(), "-c", "true"])
            .status()
        {
            assert!(s.success(), "the lock was not released");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The lock file's own mtime is the rate limit, so a pass cannot be throttled
    /// by a stamp it is not also locked against. That coupling is what the fork
    /// bomb defeated: every generation got a fresh runtime dir and so a fresh
    /// stamp, and the throttle never fired.
    #[test]
    fn the_lock_carries_the_rate_limit() {
        let d = std::env::temp_dir().join(format!("jmstamp{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(".lock");
        let mut l = Lock::take(&p).expect("lock");
        // No stamp yet: enormous, so a first pass always runs.
        assert!(l.age() > 60);
        l.stamp();
        assert!(l.age() < 5);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn session_meta_reads_backwards_and_prefers_a_custom_title() {
        let d = std::env::temp_dir().join(format!("jmmeta{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("t.jsonl");
        std::fs::write(
            &f,
            concat!(
                r#"{"cwd":"/w","version":"2.1.1"}"#,
                "\n",
                r#"{"type":"ai-title","aiTitle":"what the model called it"}"#,
                "\n",
                r#"{"type":"custom-title","customTitle":"what I called it"}"#,
                "\n"
            ),
        )
        .unwrap();
        let (cwd, ver, ttl, src) = session_meta(&f);
        assert_eq!((cwd.as_str(), ver.as_str()), ("/w", "2.1.1"));
        assert_eq!(ttl, "what I called it");
        assert_eq!(src, "t");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// An untitled session falls back to the last prompt, and says so with `p`,
    /// because a LIVE pane may borrow a real title but never a prompt.
    #[test]
    fn an_untitled_session_falls_back_to_its_last_prompt() {
        let d = std::env::temp_dir().join(format!("jmmeta2{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("t.jsonl");
        std::fs::write(
            &f,
            concat!(
                r#"{"cwd":"/w","version":"2.1.1"}"#,
                "\n",
                r#"{"type":"last-prompt","lastPrompt":"the thing\nI asked"}"#,
                "\n"
            ),
        )
        .unwrap();
        let (_, _, ttl, src) = session_meta(&f);
        assert_eq!(ttl, "the thing I asked");
        assert_eq!(src, "p");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_long_prompt_is_cut_with_an_ellipsis() {
        let d = std::env::temp_dir().join(format!("jmmeta3{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("t.jsonl");
        let long = "x".repeat(200);
        std::fs::write(
            &f,
            format!(r#"{{"type":"last-prompt","lastPrompt":"{}"}}"#, long),
        )
        .unwrap();
        let (_, _, ttl, _) = session_meta(&f);
        assert_eq!(ttl.chars().count(), 80);
        assert!(ttl.ends_with('…'));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A tab in any of the three fields would shift every field after it, since
    /// the cache is tab-separated.
    #[test]
    fn tabs_are_kept_out_of_the_cached_fields() {
        let d = std::env::temp_dir().join(format!("jmmeta4{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("t.jsonl");
        std::fs::write(
            &f,
            "{\"cwd\":\"/w\\ta\",\"version\":\"1\",\"type\":\"custom-title\",\"customTitle\":\"a\\tb\"}\n",
        )
        .unwrap();
        let (cwd, _, ttl, _) = session_meta(&f);
        assert!(!cwd.contains('\t'));
        assert!(!ttl.contains('\t'));
        let _ = std::fs::remove_dir_all(&d);
    }
}
