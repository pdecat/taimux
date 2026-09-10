//! Which version each agent is actually RUNNING.
//!
//! Read from the live process rather than from whatever is on `$PATH`, so a
//! long-lived session that a self-update has left a release or two behind stands
//! out. A port of taimux's `_agent_version` and its three sources, cheapest
//! first.
//!
//! The daemon changes one thing about this and it is the point of having one: the
//! probe is cached in memory by (path, mtime) instead of in a file per binary, so
//! a version costs a fork once per build rather than once per cold cache.

use crate::env;
use std::collections::HashMap;
use std::process::Command;
use std::time::Duration;

/// `1.2`, `1.2.3`, `v2.1.229`, `0.74.0-rc1`: a version, and not a random path
/// component that happens to contain digits.
pub fn is_version(s: &str) -> bool {
    let s = s.strip_prefix('v').unwrap_or(s);
    let mut it = s.splitn(2, '.');
    let (Some(major), Some(rest)) = (it.next(), it.next()) else {
        return false;
    };
    if major.is_empty() || !major.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    // minor, then optionally anything the semver-ish tail allows
    let minor_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if minor_end == 0 {
        return false;
    }
    let tail = &rest[minor_end..];
    tail.is_empty()
        || (matches!(tail.as_bytes()[0], b'.' | b'+' | b'-')
            && tail[1..]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-')))
}

/// The version in the running binary's own path, deepest component wins:
///   `~/.local/share/claude/versions/2.1.229`            -> 2.1.229
///   `…/mise/installs/opencode/1.18.10/opencode`         -> 1.18.10
///
/// Only trusted when the path also names the agent, so a node-wrapped CLI cannot
/// report the version of the node it runs on (`…/installs/node/25.9.0/bin/node`).
pub fn from_path(path: &str, agent: &str) -> Option<String> {
    if path.is_empty() || agent.is_empty() || !path.contains(agent) {
        return None;
    }
    path.split('/')
        .rfind(|c| is_version(c))
        .map(|c| c.strip_prefix('v').unwrap_or(c).to_string())
}

/// The `version` of the nearest `package.json` above a `.js` entry point, which
/// is how a node-wrapped CLI says what it is.
pub fn from_script(argv: &str) -> Option<String> {
    for tok in argv.split(' ') {
        if !(tok.starts_with('/')
            && (tok.ends_with(".js") || tok.ends_with(".mjs") || tok.ends_with(".cjs")))
        {
            continue;
        }
        let mut dir = std::path::Path::new(tok).parent();
        while let Some(d) = dir {
            if let Ok(text) = std::fs::read_to_string(d.join("package.json")) {
                // only the head, the way the bash version reads 60 lines: the
                // top-level "version" is near the top and a lockfile-sized file
                // is not worth parsing
                if let Some(v) = crate::json::first(&text, "version") {
                    return Some(v);
                }
            }
            dir = d.parent();
        }
    }
    None
}

/// Asking the binary, which is the last resort because it costs a fork.
///
/// Never a launcher or an interpreter: only a binary whose own filename IS the
/// agent, or `node --version` would answer for gemini.
pub struct Prober {
    cache: HashMap<(String, u64), Option<String>>,
}

impl Default for Prober {
    fn default() -> Self {
        Self::new()
    }
}

impl Prober {
    pub fn new() -> Self {
        Prober {
            cache: HashMap::new(),
        }
    }

    pub fn probe(&mut self, exe: &str, agent: &str) -> Option<String> {
        if exe.is_empty() || exe.rsplit('/').next()? != agent {
            return None;
        }
        // mtime keys the cache: an in-place rewrite has to invalidate it, which
        // is exactly what a self-update in place is.
        let mtime = std::fs::metadata(exe)
            .ok()?
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        let key = (exe.to_string(), mtime);
        if let Some(hit) = self.cache.get(&key) {
            return hit.clone();
        }
        // Counted and timed: this is the only fork on the row path that runs an
        // AGENT rather than tmux, so it is the one that can cost seconds each,
        // and a refresh that pays it once per pane is what a slow refresh looks
        // like from the outside.
        let out = crate::stat::probe(|| Command::new(exe).arg("--version").output()).ok();
        let found = out
            .filter(|o| o.status.success())
            .and_then(|o| first_version(&String::from_utf8_lossy(&o.stdout)));
        self.cache.insert(key, found.clone());
        found
    }
}

/// The first thing shaped like a version in a line of `--version` output.
pub fn first_version(text: &str) -> Option<String> {
    let line = text.lines().next()?;
    let bytes: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], '.' | '+' | '-'))
            {
                i += 1;
            }
            let cand: String = bytes[start..i].iter().collect();
            // trim trailing punctuation the scan may have swept up
            let cand = cand.trim_end_matches(['.', '-', '+']).to_string();
            if is_version(&cand) {
                return Some(cand);
            }
        } else {
            i += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_shaped_strings_are_recognised() {
        for s in [
            "1.2",
            "1.2.3",
            "v2.1.229",
            "0.74.0-rc1",
            "1.18.10",
            "2.1.229",
        ] {
            assert!(is_version(s), "{s} should be a version");
        }
    }

    #[test]
    fn path_components_that_merely_contain_digits_are_not() {
        for s in [
            "",
            "claude",
            "x86_64",
            "1",
            "v",
            "lib64",
            "2026",
            "node_modules",
        ] {
            assert!(!is_version(s), "{s} should not be a version");
        }
    }

    #[test]
    fn the_deepest_version_in_the_path_wins() {
        assert_eq!(
            from_path("/home/u/.local/share/claude/versions/2.1.229", "claude").as_deref(),
            Some("2.1.229")
        );
        assert_eq!(
            from_path(
                "/home/u/.mise/installs/opencode/1.18.10/opencode",
                "opencode"
            )
            .as_deref(),
            Some("1.18.10")
        );
    }

    #[test]
    fn a_path_that_does_not_name_the_agent_is_refused() {
        // else a node-wrapped CLI reports the version of the node it runs on
        assert_eq!(
            from_path("/home/u/.mise/installs/node/25.9.0/bin/node", "gemini"),
            None
        );
    }

    #[test]
    fn a_leading_v_is_dropped() {
        assert_eq!(
            from_path("/opt/agy/v1.0.14/agy", "agy").as_deref(),
            Some("1.0.14")
        );
    }

    #[test]
    fn a_version_is_found_in_a_line_of_version_output() {
        assert_eq!(
            first_version("0.74.0 (6765f464)").as_deref(),
            Some("0.74.0")
        );
        assert_eq!(
            first_version("claude 2.1.252\n").as_deref(),
            Some("2.1.252")
        );
        assert_eq!(first_version("mise WARN something\n").as_deref(), None);
        assert_eq!(first_version("").as_deref(), None);
    }

    #[test]
    fn a_package_json_version_is_read_without_a_json_parser() {
        let text = r#"{"name": "x", "version": "0.41.2", "deps": {"version": "9.9.9"}}"#;
        assert_eq!(
            crate::json::first(text, "version").as_deref(),
            Some("0.41.2")
        );
        assert_eq!(crate::json::first("{}", "version"), None);
        // a spaced-out file is still read
        assert_eq!(
            crate::json::first("{\n  \"version\"  :   \"1.2.3\"\n}", "version").as_deref(),
            Some("1.2.3")
        );
    }

    #[test]
    fn a_launcher_is_never_probed() {
        let mut p = Prober::new();
        // the filename must BE the agent, or `node --version` answers for gemini
        assert_eq!(p.probe("/usr/bin/node", "gemini"), None);
        assert_eq!(p.probe("", "claude"), None);
    }
}

/// What a session started RIGHT NOW would run: the version the launcher symlink
/// currently points at.
///
/// A pane behind this one is running code a self-update has already replaced, and
/// it is the row ctrl-x acts on, which is why the list paints that version
/// yellow. Prints nothing when the launcher is missing or points somewhere
/// unexpected, and the list then marks nothing rather than marking everything: an
/// unknown installed version is not evidence that a pane is behind.
///
/// Only claude publishes a launcher taimux knows how to read, so only claude
/// rows are ever marked.
pub fn installed_claude(home: &str) -> Option<String> {
    if !env::on("TAIMUX_VERSIONS") {
        return None;
    }
    let target = std::fs::canonicalize(format!("{}/.local/bin/claude", home)).ok()?;
    let vdir = format!("{}/.local/share/claude/versions/", home);
    let s = target.to_string_lossy();
    s.strip_prefix(&vdir)
        .filter(|rest| !rest.is_empty() && !rest.contains('/'))
        .map(|rest| rest.to_string())
}

#[cfg(test)]
mod installed_tests {
    use super::*;

    /// A launcher pointing outside the versions directory says nothing, and a
    /// missing one says nothing either. Both have to read as "unknown" rather
    /// than as "every pane is behind".
    #[test]
    fn only_a_launcher_inside_the_versions_dir_answers() {
        let home = std::env::temp_dir().join(format!("jmver{}", std::process::id()));
        let bin = home.join(".local/bin");
        let vers = home.join(".local/share/claude/versions");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&vers).unwrap();
        let h = home.to_string_lossy().to_string();

        assert_eq!(installed_claude(&h), None); // no launcher at all

        std::fs::write(vers.join("2.1.258"), "x").unwrap();
        std::os::unix::fs::symlink(vers.join("2.1.258"), bin.join("claude")).unwrap();
        assert_eq!(installed_claude(&h), Some("2.1.258".into()));

        // pointing somewhere else entirely: unknown, not "behind"
        std::fs::write(home.join("elsewhere"), "x").unwrap();
        std::fs::remove_file(bin.join("claude")).unwrap();
        std::os::unix::fs::symlink(home.join("elsewhere"), bin.join("claude")).unwrap();
        assert_eq!(installed_claude(&h), None);

        let _ = std::fs::remove_dir_all(&home);
    }
}
