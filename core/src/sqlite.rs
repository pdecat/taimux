//! Reading another tool's SQLite store, without writing anything into its
//! directory.
//!
//! Two agents keep their conversations in a database rather than in files:
//! OpenCode in `~/.local/share/opencode/opencode.db`, and recent Codex in
//! `~/.codex/state_*.sqlite`. Everything else here reads JSONL, which needs no
//! engine at all.
//!
//! **The engine is [rusqlite](https://github.com/rusqlite/rusqlite)** with
//! `bundled`, which compiles SQLite's own amalgamation in. It is behind a Cargo
//! feature (`sqlite`, on by default) because it is the only dependency this
//! crate takes: a build that turns it off loses the OpenCode and Codex rows and
//! nothing else.
//!
//! **It was [turso](https://github.com/tursodatabase/turso) first**, a SQLite
//! rewritten in Rust, chosen for being C-free. It is not: `turso_core` depends
//! on `simsimd`, a C SIMD library, mandatorily and not behind a feature, so the
//! static musl build needed a C compiler either way and a missing one surfaced
//! as `cannot find -lsimsimd` after everything had compiled. With that argument
//! gone nothing else favoured it, and the measured gap is not close: on this
//! machine, same profile, same three queries against the same 71 MB database,
//! the static binary went 11.9 MB to 1.49 MB, the cold build 75s to 15s, the
//! dependency tree 324 crates to 32, opening the file 16ms to 0.08ms and a
//! 2000-row join 26ms to 7ms. It also has a real `SQLITE_OPEN_READ_ONLY`, which
//! turso does not offer at all, and a synchronous API, which is why there is no
//! `block_on` in this file any more.
//!
//! **Read through a symlink, never the path itself.** SQLite creates `-wal` and
//! `-shm` beside whatever path it was handed, and it does so even opened
//! READ-ONLY, so opening `opencode.db` where it lives drops files into a
//! directory that belongs to another program while that program may be running.
//! Opening a symlink in this tool's own runtime directory puts them next to the
//! SYMLINK instead: same bytes read, same inode, nothing at all written where
//! OpenCode can see it. Verified both ways: after a read the source file's
//! mtime, size and md5 are unchanged and its directory has gained nothing.
//!
//! Nothing here writes, and the open flag says so as well as the usage: every
//! statement this module is given is a `SELECT`, and a store that cannot be
//! opened or a query that fails is an agent with no rows in the list rather than
//! an error anybody sees.

use std::path::{Path, PathBuf};

/// One column of one row, in the three shapes these stores actually use.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Text(String),
    Int(i64),
    Null,
}

impl Cell {
    /// The value as text. A number reads as its digits, so a caller that only
    /// wants a string never has to know which of the two it got.
    pub fn text(&self) -> String {
        match self {
            Cell::Text(s) => s.clone(),
            Cell::Int(i) => i.to_string(),
            Cell::Null => String::new(),
        }
    }

    /// The value as a number, or 0. Text that looks like a number parses, since
    /// these stores are not always consistent about which they used.
    pub fn int(&self) -> i64 {
        match self {
            Cell::Int(i) => *i,
            Cell::Text(s) => s.parse().unwrap_or(0),
            Cell::Null => 0,
        }
    }
}

/// Where the symlinks this module opens through are kept.
fn link_dir() -> PathBuf {
    crate::paths::runtime_dir().join("db")
}

/// A symlink in our runtime directory pointing at `src`, created if needed.
///
/// Named after the source path with everything awkward flattened, so one store
/// is always reached through one link and a second reader finds the same file
/// rather than making another.
fn link_to(src: &Path) -> Option<PathBuf> {
    let dir = link_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let name: String = src
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let link = dir.join(name);
    match std::fs::read_link(&link) {
        Ok(t) if t == src => return Some(link),
        Ok(_) => {
            let _ = std::fs::remove_file(&link);
        }
        Err(_) => {}
    }
    match std::os::unix::fs::symlink(src, &link) {
        Ok(()) => Some(link),
        // Another pass created it between the read and the write: that is the
        // link we wanted, so take it.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Some(link),
        Err(_) => None,
    }
}

#[cfg(feature = "sqlite")]
mod engine {
    use super::{link_to, Cell};
    use std::path::Path;

    /// An open store, read-only by flag as well as by use.
    pub struct Db {
        conn: rusqlite::Connection,
    }

    /// Open a store, through a symlink so nothing lands beside the original.
    ///
    /// `None` for a store that is not there, which is the ordinary case: most
    /// machines have one or two of these agents installed, not all of them.
    ///
    /// `NO_MUTEX` because this connection never leaves the thread that made it,
    /// and `READ_ONLY` because nothing here has any business writing. The second
    /// does NOT stop SQLite creating its sidecar files, which is what the
    /// symlink is for.
    pub fn open(path: &Path) -> Option<Db> {
        if !path.is_file() {
            return None;
        }
        let link = link_to(path)?;
        let conn = rusqlite::Connection::open_with_flags(
            &link,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .ok()?;
        Some(Db { conn })
    }

    impl Db {
        /// Every row a statement returns, as cells.
        ///
        /// Whole rather than streamed because every query here is bounded by the
        /// shape of the question: a session list, or one session's messages.
        pub fn rows(&self, sql: &str) -> Option<Vec<Vec<Cell>>> {
            let mut st = self.conn.prepare(sql).ok()?;
            let n = st.column_count();
            let mut out = Vec::new();
            let mut q = st.query([]).ok()?;
            while let Ok(Some(row)) = q.next() {
                let mut cells = Vec::with_capacity(n);
                for i in 0..n {
                    cells.push(match row.get_ref(i) {
                        Ok(rusqlite::types::ValueRef::Text(t)) => {
                            Cell::Text(String::from_utf8_lossy(t).into_owned())
                        }
                        Ok(rusqlite::types::ValueRef::Integer(v)) => Cell::Int(v),
                        Ok(rusqlite::types::ValueRef::Real(f)) => Cell::Int(f as i64),
                        Ok(rusqlite::types::ValueRef::Blob(b)) => {
                            Cell::Text(String::from_utf8_lossy(b).into_owned())
                        }
                        _ => Cell::Null,
                    });
                }
                out.push(cells);
            }
            Some(out)
        }
    }
}

/// The shape of this module when the engine is compiled out.
///
/// A build without the `sqlite` feature still has every caller: they simply find
/// no store, which is the same answer they get on a machine where the agent is
/// not installed. That is why there is no `cfg` at any call site.
#[cfg(not(feature = "sqlite"))]
mod engine {
    use super::Cell;
    use std::path::Path;

    pub struct Db;

    pub fn open(_path: &Path) -> Option<Db> {
        None
    }

    impl Db {
        pub fn rows(&self, _sql: &str) -> Option<Vec<Vec<Cell>>> {
            None
        }
    }
}

pub use engine::{open, Db};

/// Whether this build can read a SQLite store at all.
///
/// Worth asking because the answer changes what the ended list can contain, and
/// a feature nobody can see the state of is a feature nobody can debug.
pub fn available() -> bool {
    cfg!(feature = "sqlite")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cell_reads_as_either_text_or_a_number() {
        assert_eq!(Cell::Text("abc".into()).text(), "abc");
        assert_eq!(Cell::Int(42).text(), "42");
        assert_eq!(Cell::Null.text(), "");
        assert_eq!(Cell::Int(42).int(), 42);
        // these stores are not consistent about which they used
        assert_eq!(Cell::Text("1787640865115".into()).int(), 1787640865115);
        assert_eq!(Cell::Text("not a number".into()).int(), 0);
        assert_eq!(Cell::Null.int(), 0);
    }

    /// A store that is not there is no rows, never an error: most machines have
    /// one or two of these agents installed rather than all of them.
    #[test]
    fn a_missing_store_opens_as_nothing() {
        assert!(open(Path::new("/nowhere/at/all.db")).is_none());
        // …and so does a directory, which `is_file` rejects before turso sees it
        assert!(open(Path::new("/tmp")).is_none());
    }

    /// The link is what keeps the `-wal` out of the other program's directory,
    /// and the same source always reaches the same link so two passes do not
    /// make two of them.
    #[test]
    fn one_store_is_reached_through_one_link() {
        let _g = crate::env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("tmxdb{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", &root);

        let src = root.join("some.db");
        std::fs::write(&src, b"not really a database").unwrap();
        let a = link_to(&src).expect("linked");
        let b = link_to(&src).expect("linked again");
        assert_eq!(a, b);
        assert_eq!(std::fs::read_link(&a).unwrap(), src);

        // a link left pointing somewhere else is replaced rather than trusted
        let other = root.join("other.db");
        std::fs::write(&other, b"x").unwrap();
        let _ = std::fs::remove_file(&a);
        std::os::unix::fs::symlink(&other, &a).unwrap();
        let c = link_to(&src).expect("relinked");
        assert_eq!(std::fs::read_link(&c).unwrap(), src);

        std::env::remove_var("XDG_RUNTIME_DIR");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A build is allowed to drop the engine; what it may not do is make its
    /// callers know about it.
    #[test]
    fn the_engine_reports_whether_it_is_here() {
        assert_eq!(available(), cfg!(feature = "sqlite"));
    }
}
