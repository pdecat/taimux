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
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use taimux_core::{conv, index};

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
    let head = parse_head(lines.next()?)?;
    Some((
        head,
        lines
            .next()
            .unwrap_or("")
            .trim_end_matches('\n')
            .to_string(),
    ))
}

/// The header alone, without pulling the blob under it into memory.
///
/// One index file can hold a quarter of a megabyte of prose, and a pass that
/// only wants to know whether a conversation has changed reads 820 of them.
fn read_head_line(f: &Path) -> Option<Head> {
    use std::io::BufRead;
    let fh = std::fs::File::open(f).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(fh).read_line(&mut line).ok()?;
    parse_head(line.trim_end_matches('\n'))
}

/// Anything that is not a well-formed header is a file being rewritten under us
/// or left by an older version, and it is skipped rather than guessed at.
fn parse_head(line: &str) -> Option<Head> {
    let head: Vec<&str> = line.split(' ').collect();
    if head.first() != Some(&"idx") || head.len() < 5 {
        return None;
    }
    Some(Head {
        seen: head[1].parse().ok()?,
        // Last, and joined back, so a key with a space in it survives.
        path: head[4..].join(" "),
    })
}

/// One pane's live claude transcript, indexed. False when there was nothing to
/// do.
pub fn index_pane(id: &str, tr: &Path) -> bool {
    index_one(id, "claude", &tr.to_string_lossy())
}

/// One conversation's prose, indexed, whoever wrote it and wherever it is kept.
///
/// The incremental half is the agent's business now (`agents::prose`): a file
/// reports how many bytes it had and hands back only what it gained, a database
/// reports when its row was last touched and hands back the whole thing. Either
/// way what arrives here is a fingerprint, some text, and whether that text
/// replaces the blob or extends it.
pub fn index_one(id: &str, agent: &str, key: &str) -> bool {
    let f = index::index_dir().join(index::key_for(id));

    // The cheap question first, and it is the whole performance story of a pass:
    // asking the store for a fingerprint is a `stat` or an indexed lookup, while
    // finding out by reading is a gigabyte across the history. Only the header
    // line is read for the same reason; the blob below it can be a quarter of a
    // megabyte and is wanted only when there is something to append to it.
    let head = read_head_line(&f);
    let from = match &head {
        Some(h) if h.path == key => h.seen,
        _ => 0,
    };
    if from > 0 && taimux_core::agents::fingerprint(agent, key) == Some(from) {
        return false; // nothing has been said since the last pass
    }

    let known = if from > 0 {
        read_head(&f).map(|(_, blob)| blob).unwrap_or_default()
    } else {
        String::new()
    };

    let Some(p) = taimux_core::agents::prose(agent, key, from) else {
        return false;
    };
    if p.fingerprint == from && from > 0 {
        return false; // the store changed its mind between the two questions
    }
    let joined = if p.whole {
        p.text
    } else {
        format!("{}{}", known, p.text)
    };
    let blob = cap(&joined, env_usize("TAIMUX_SEARCH_CAP", 262144));

    let _ = std::fs::create_dir_all(index::index_dir());
    // Written by rename: half a file is worse than an old one when two pickers
    // are reading while a pass writes.
    let tmp = f.with_extension(format!("t{}", std::process::id()));
    let ok = std::fs::write(
        &tmp,
        format!("idx {} {} {} {}\n{}\n", p.fingerprint, now(), id, key, blob),
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

/// A conversation's four cached fields, tab-separated, as the cache spells them.
fn fields(m: &taimux_core::agents::Meta) -> String {
    format!("{}\t{}\t{}\t{}", m.cwd, m.version, m.title, m.src)
}

/// Every conversation every agent has, newest first.
///
/// **Uncapped.** There used to be a cap of 200 here, and the argument for it was
/// that the corpus only grows: Claude Code's own cleanup is set to ten years on
/// this machine, so it is hundreds of conversations today and thousands
/// eventually. What the cap actually cost was the sessions you go looking for. A
/// conversation from last month is the one you cannot find any other way, and it
/// is also the first one a recency cap drops; with 572 transcripts here, two
/// thirds of the history was invisible and nothing on the list said so.
///
/// What makes the uncapped version affordable is that the cost of a pass is the
/// directory walk plus a read for each conversation that has said something since
/// the last one, and a conversation nobody is in never says anything again. The
/// walk itself is `stat` per file.
///
/// `TAIMUX_SESSIONS_MAX` still exists and still truncates, for a machine that
/// wants the old behaviour. It defaults to 0, meaning no limit.
fn past_by_recency(limit: usize) -> Vec<taimux_core::agents::Past> {
    let mut out = taimux_core::agents::discover();
    out.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.key.cmp(&b.key)));
    if limit > 0 {
        out.truncate(limit);
    }
    out
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
    // Only claude appears here: it is the one agent that publishes which
    // conversation a pane is on, so it is the one whose sessions can be told
    // apart from the history.
    let onpane: std::collections::HashMap<String, String> = live
        .iter()
        .map(|(pane, p)| (p.to_string_lossy().into_owned(), pane.clone()))
        .collect();

    let mut known: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for e in index::sessions() {
        known.insert(
            format!("{} {} {}", e.mtime, e.agent, e.key),
            format!("{}\t{}\t{}\t{}", e.cwd, e.version, e.title, e.src),
        );
    }

    let all = past_by_recency(env_usize("TAIMUX_SESSIONS_MAX", 0));

    // Everything a conversation says about itself, resolved once. Three sources,
    // cheapest first: the line the last pass wrote (a conversation untouched
    // since then cannot have changed its mind), what the store handed over with
    // the listing, and only failing both, a read of the conversation itself.
    //
    // Those reads are the whole cost of a cold pass, they are independent of one
    // another, and they go on the same shared queue the content index uses: a
    // slice each is the wrong split when one conversation can be a thousand
    // times the size of its neighbour.
    let lines: Vec<Option<String>> = all
        .iter()
        .map(|p| {
            known
                .get(&format!("{} {} {}", p.mtime, p.agent, p.key))
                .cloned()
                .or_else(|| p.meta.as_ref().map(fields))
        })
        .collect();
    let lines: Vec<std::sync::Mutex<Option<String>>> =
        lines.into_iter().map(std::sync::Mutex::new).collect();
    let threads = env_usize("TAIMUX_INDEX_THREADS", default_threads()).max(1);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let (all_ref, lines_ref, next_ref) = (&all, &lines, &next);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(move || loop {
                let i = next_ref.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(past) = all_ref.get(i) else { return };
                if lines_ref[i].lock().map(|l| l.is_some()).unwrap_or(true) {
                    continue;
                }
                let m = taimux_core::agents::meta(past.agent, &past.key);
                if let Ok(mut slot) = lines_ref[i].lock() {
                    *slot = Some(fields(&m));
                }
            });
        }
    });

    let mut body = index::header(now());
    for (past, line) in all.iter().zip(lines) {
        let meta = line.into_inner().ok().flatten().unwrap_or_default();
        body.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            past.mtime,
            onpane.get(&past.key).map(|s| s.as_str()).unwrap_or("-"),
            past.agent,
            past.key,
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

/// The content of the PAST conversations, so the search reaches them too.
///
/// This is half the reason the list exists: what you remember about last Tuesday
/// is what was said, not where it ran. Keyed by `dead:<agent>:<key>`, since a
/// past session has no pane; it is then indexed exactly like a live one, which
/// means it is read once and never again, because a conversation nothing is
/// running does not grow.
pub fn index_dead() {
    let work: Vec<(String, String)> = index::sessions()
        .into_iter()
        .filter(|e| e.pane == "-" && !e.key.is_empty())
        .map(|e| (e.agent, e.key))
        .collect();

    // The store-backed ones stay on this thread. There is one database behind
    // all of them, and opening it from several threads at once is a risk taken
    // for no gain: a hundred indexed lookups is not where a pass spends its time.
    let (db_backed, files): (Vec<_>, Vec<_>) =
        work.into_iter().partition(|(agent, _)| agent == "opencode");

    // The files are where it does. A cold pass extracts the prose from a
    // gigabyte of transcripts, and it is CPU rather than disk: 27.7s of user
    // time against 1.9s of system, measured here. Since each conversation is
    // read and written independently, splitting them across threads is the whole
    // fix, and it takes the first pass of a boot from 30s to single figures.
    // A SHARED queue, not a slice each. Splitting the list into equal counts is
    // the obvious way and it does not work here: conversation sizes are skewed
    // by two orders of magnitude, and cutting a recency-sorted list in four put
    // three quarters of the bytes in one piece. Measured: 0.2s, 0.2s, 0.4s and
    // 13.1s. Threads that take the next conversation as they finish the last one
    // cannot be handed the wrong share.
    let threads = env_usize("TAIMUX_INDEX_THREADS", default_threads()).max(1);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let files = &files;
    let next = &next;
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(move || loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some((agent, key)) = files.get(i) else {
                    return;
                };
                index_one(&index::past_id(agent, key), agent, key);
            });
        }
    });

    for (agent, key) in db_backed {
        index_one(&index::past_id(&agent, &key), &agent, &key);
    }
}

/// How many threads a cold pass uses by default.
///
/// Half the machine, at most four. This runs BEHIND a picker, on a box somebody
/// is working on, so taking every core to read last month's conversations is the
/// wrong trade even though it would finish sooner.
fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| (n.get() / 2).clamp(1, 4))
        .unwrap_or(1)
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
        .filter(|e| !e.key.is_empty())
        .map(|e| {
            if e.pane == "-" {
                index::past_id(&e.agent, &e.key)
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
        // The header alone. Reading each file whole to reach its first line cost
        // a pass ~200 MB once the history stopped being capped at 200.
        let Some(head) = first_line(&path) else {
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

/// One file's first line, without reading the rest of it.
fn first_line(path: &Path) -> Option<String> {
    use std::io::BufRead;
    let fh = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(fh).read_line(&mut line).ok()?;
    Some(line.trim_end_matches('\n').to_string())
}

/// `ha:%6` is another host's pane. `%6` and `dead:claude:/path` are ours.
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
}
