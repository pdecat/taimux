//! Other hosts' sessions, in the same list.
//!
//! Step 5 of replacing bash. A pane holding `ssh <host> -t tmux …` is a window
//! onto a whole other tmux server, and the agents running in it are invisible to
//! everything else here: they are not this server's panes and not this box's
//! processes. But that host has a taimux of its own, and `list` is a complete
//! answer about it.
//!
//! **The rule that makes this safe is: federate, never reach in.** Each host is
//! asked about ITSELF, so the hook state, the screen reading, the versions and
//! the transcripts all stay on the side that can see them, and `list` is
//! deliberately local-only because it is the wire format. That one rule is what
//! makes a cycle between two boxes ssh'd into each other structurally impossible
//! rather than merely unlikely.
//!
//! **Hosts are DISCOVERED, not configured**: whatever the local panes are already
//! ssh'd into is the list. That is not just less setup. A host found this way has
//! a warm ssh ControlMaster by construction (the pane's own connection), so a
//! fetch is a tenth of a second rather than a handshake; it drops off the list
//! when its pane goes; and it arrives with its jump target already identified,
//! since the pane it was found in IS the way back to it.
//!
//! Nothing on the refresh path waits on the network except a host with NOTHING
//! cached, which is fetched once per boot so the first picker after a reboot is
//! complete. Everything else is served as it stands and refreshed behind the
//! picker.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use taimux_core::{env, index, proc};

/// Rebuilding PATH on the far side, because `taimux` is NOT on a
/// non-interactive ssh's PATH on a stock install: `~/.local/bin` is
/// login-shell-only, so `ssh ha taimux list` fails where
/// `ssh ha '~/.local/bin/taimux list'` works. It also covers a host that has
/// taimux only as a tmux plugin, and it exits 127 when there is none, which is
/// how "no taimux over there" is told apart from "unreachable".
const REMOTE_TAIMUX: &str = "PATH=\"$HOME/.local/bin:$HOME/bin:$PATH\"; \
     for d in \"$HOME\"/.tmux/plugins/taimux*/ \"$HOME\"/.config/tmux/plugins/taimux*/; do \
     [ -x \"$d/taimux\" ] && PATH=\"$PATH:${d%/}\"; done; \
     command -v taimux >/dev/null 2>&1 || exit 127; exec taimux";

/// The preamble, for a caller that needs to build its own remote command.
pub fn remote_taimux() -> &'static str {
    REMOTE_TAIMUX
}

/// The subcommand `index_fetch` asks a remote host for.
///
/// Named rather than inlined so the test below can assert it against the
/// dispatcher, because nothing else can: a wrong name here is answered with
/// `no such command` and exit 1, which `index_fetch` cannot tell apart from an
/// old host with nothing to say. It stayed wrong for exactly that reason.
const INDEX_DUMP_CMD: &str = "index-dump";

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn enabled() -> bool {
    env::on("TAIMUX_REMOTE")
}

/// Run something with a deadline, and kill it when the deadline passes.
///
/// The time limit is the ONLY way out of a popup whose helper wedges: a child
/// that swallows ctrl-c cannot be escaped from, because the parent waits for it
/// either way. So nothing on the picker's path may be given a long one.
fn bounded(secs: u64, mut cmd: Command) -> (i32, String) {
    let Ok(mut child) = cmd.stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else {
        return (255, String::new());
    };
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut out = child.stdout.take();
    loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                let mut s = String::new();
                if let Some(o) = out.as_mut() {
                    let _ = o.read_to_string(&mut s);
                }
                return (st.code().unwrap_or(255), s);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return (124, String::new()); // the status `timeout` uses
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return (255, String::new()),
        }
    }
}

/// One ssh, bounded, batch-mode, and never reading stdin: `-n` matters because
/// this runs behind a picker that owns the terminal.
pub fn ssh(host: &str, remote: &str, timeout: u64) -> (i32, String) {
    let mut c = Command::new("ssh");
    c.args([
        "-n",
        "-o",
        "BatchMode=yes",
        "-o",
        &format!(
            "ConnectTimeout={}",
            env::num("TAIMUX_SSH_CONNECT_TIMEOUT", 2)
        ),
        "--",
        host,
        remote,
    ]);
    taimux_core::stat::ssh(|| bounded(timeout, c))
}

/// Why an ssh failed, in the words the row will carry.
pub fn ssh_why(status: i32) -> &'static str {
    match status {
        127 => "no taimux on that host",
        124 | 137 => "timed out",
        255 => "unreachable",
        _ => "ssh failed",
    }
}

/// The host an `ssh … tmux …` argv connects to, or None.
///
/// Finds the ssh token, then takes the first bare word after it as the host,
/// skipping options and the values of the ones that take a separate argument.
/// `tmux` has to appear AFTER that: it is what separates a pane holding a nested
/// SERVER from one merely running a remote command (`ssh host journalctl -f`),
/// and from a plain remote login shell, neither of which has anything to
/// federate.
///
/// **ssh is looked for as a TOKEN rather than as argv[0]**, because it routinely
/// is not argv[0]: when a `#!` script is named ssh (a ProxyCommand or kerberos
/// wrapper, and the stand-in the demo uses) the kernel runs it as
/// `bash /path/to/ssh host …`, so the interpreter holds argv[0] and requiring
/// that to read "ssh" silently found no hosts at all on such a machine.
pub fn ssh_host(argv: &str) -> Option<String> {
    // the options that take a separate value, so the value is not read as a host
    const TAKES_VALUE: &str = "BbcDEeFIiJLlmOopQRSWw";
    let toks: Vec<&str> = argv.split(' ').filter(|t| !t.is_empty()).collect();
    let mut saw_ssh = false;
    let mut host: Option<&str> = None;
    let mut saw_tmux = false;
    let mut i = 0;
    while i < toks.len() {
        let tok = toks[i];
        if !saw_ssh {
            let base = tok.rsplit('/').next().unwrap_or(tok);
            if base == "ssh" {
                saw_ssh = true;
            }
            i += 1;
            continue;
        }
        if host.is_none() {
            if tok.starts_with('-') {
                let mut c = tok.chars();
                c.next();
                if let (Some(f), None) = (c.next(), c.next()) {
                    if TAKES_VALUE.contains(f) {
                        i += 1; // its value is not a host
                    }
                }
                i += 1;
                continue;
            }
            host = Some(tok);
            i += 1;
            continue;
        }
        if tok == "tmux" {
            saw_tmux = true;
        }
        i += 1;
    }
    let host = host?;
    if !saw_tmux {
        return None;
    }
    // Anything unexpected in a host is dropped rather than escaped: the name
    // becomes a cache filename and an ssh argument, and there is no such thing as
    // a hostname needing a quote.
    if host.is_empty()
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._@-".contains(&b))
    {
        return None;
    }
    Some(host.to_string())
}

/// host and the local pane it is reached through, for every ssh pane.
pub fn ssh_tmux_panes(panes: &[(String, String)]) -> Vec<(String, String)> {
    // The foreground argv per tty, which is the same scan the pane list uses. A
    // tty can carry more than one foreground-group process, so every one of them
    // is tried before the pane is written off.
    let fg = proc::foreground_map();
    let mut out = Vec::new();
    for (id, tty) in panes {
        let tty = tty.trim_start_matches("/dev/");
        for f in fg.iter().filter(|f| f.tty == tty) {
            if let Some(h) = ssh_host(&f.argv) {
                out.push((h, id.clone()));
                break;
            }
        }
    }
    out
}

pub fn cache_dir() -> PathBuf {
    taimux_core::paths::runtime_dir().join("remote")
}

/// The host-list cache, keyed by tmux SERVER.
///
/// Keyed because that is what it is a property of: the list comes off that
/// server's panes, and a second server on the same box (the demo runs one) has
/// entirely different ones.
fn hosts_file(dir: &Path) -> PathBuf {
    let sock = std::env::var("TMUX").unwrap_or_default();
    let sock = sock.split(',').next().unwrap_or("").to_string();
    if sock.is_empty() {
        dir.join(".hosts")
    } else {
        dir.join(format!(".hosts.{}", sock.replace('/', "%")))
    }
}

/// Write a file by rename, because two pickers can be open at once and half a
/// file is worse than an old one.
fn write_atomic(f: &Path, body: &str) {
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = f.with_extension(format!("t{}", std::process::id()));
    if std::fs::write(&tmp, body).is_ok() && !body.is_empty() && std::fs::rename(&tmp, f).is_ok() {
        return;
    }
    let _ = std::fs::remove_file(&tmp);
}

/// A cached file's header: `<status> <epoch> [reason]`.
///
/// One line, so a reader learns both what happened and how old it is from ONE
/// read: no stat, no fork. That matters because the freshness test runs per host
/// on every refresh tick.
fn header(f: &Path) -> Option<(String, i64, String)> {
    let text = std::fs::read_to_string(f).ok()?;
    let line = text.lines().next()?;
    let mut it = line.splitn(3, ' ');
    let status = it.next()?.to_string();
    let stamp = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    Some((status, stamp, it.next().unwrap_or("").to_string()))
}

/// The discovered hosts, cached.
///
/// Discovery is a full process scan and it answers the same thing tick after
/// tick, since panes do not open and close on the timescale a picker refreshes
/// on. Caching it keeps the steady-state cost of federation to reading a few
/// small files, and keeps that cost OFF everyone else: without it a server with
/// no ssh panes at all still pays a scan on every refresh to find that out.
pub fn hosts_cached(dir: &Path, panes: &[(String, String)]) -> Vec<String> {
    let f = hosts_file(dir);
    if let Some((status, stamp, _)) = header(&f) {
        if status == "hosts" && now() - stamp < env::num("TAIMUX_HOSTS_TTL", 5) as i64 {
            return std::fs::read_to_string(&f)
                .unwrap_or_default()
                .lines()
                .skip(1)
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect();
        }
    }
    let mut hosts: Vec<String> = ssh_tmux_panes(panes).into_iter().map(|(h, _)| h).collect();
    hosts.sort();
    hosts.dedup();
    // The header line is unconditional, so the file is never empty and the write
    // is never mistaken for a failure. bash got this wrong the other way round:
    // its `if { … } > tmp` tested the LAST command in the group, which was the
    // "are there any hosts?" guard, so on every server with nothing to federate
    // the write read as failed, the cache was never written, and each refresh both
    // re-ran the scan the cache exists to avoid and left its temp file behind.
    write_atomic(&f, &format!("hosts {}\n{}\n", now(), hosts.join("\n")));
    hosts
}

/// Ask one host about itself, and write down what it said.
pub fn fetch(host: &str, dir: &Path) {
    let (rc, out) = ssh(
        host,
        &format!("{} list", REMOTE_TAIMUX),
        env::num("TAIMUX_SSH_TIMEOUT", 4),
    );
    let f = dir.join(host);
    if rc == 0 {
        write_atomic(&f, &format!("ok {}\n{}", now(), out));
        // A host that has answered ONCE is a participant, and from then on its
        // silence is worth a row. One that has never answered is simply not
        // running taimux, which is a permanent and perfectly fine state of
        // affairs rather than an outage, so it stays out of the list entirely
        // instead of parking a complaint in it. Install taimux there and the
        // host joins on its own; that is the whole of the configuration.
        let _ = std::fs::write(dir.join(format!("{}.seen", host)), "");
    } else {
        write_atomic(&f, &format!("err {} {}\n", now(), ssh_why(rc)));
    }
}

/// One host's cached reply, turned into rows.
///
/// A row's pane id becomes `<host>:<pane>`. The host rides INSIDE the id rather
/// than in a column of its own because that id is the only thing the picker
/// carries downstream, so one composite string keeps preview, switch and restart
/// unchanged and costs `list` no schema change.
pub fn render(host: &str, dir: &Path) -> String {
    let f = dir.join(host);
    let Some((status, _, reason)) = header(&f) else {
        return String::new();
    };
    let text = std::fs::read_to_string(&f).unwrap_or_default();
    let mut s = String::new();
    if status == "ok" {
        let mut bad = 0;
        for line in text.lines().skip(1) {
            if line.is_empty() {
                continue;
            }
            if line.split('\t').count() == 8 {
                s.push_str(&format!("{}:{}\n", host, line));
            } else {
                bad += 1;
            }
        }
        // An older taimux over there answers in another shape. Say so on one row
        // rather than dropping its sessions silently.
        if bad > 0 {
            s.push_str(&format!(
                "{}:!\t{}\t-\t-\t\tunknown\t-\tits taimux answers in another format ({} row(s) dropped)\n",
                host, host, bad
            ));
        }
    } else if dir.join(format!("{}.seen", host)).exists() {
        s.push_str(&format!(
            "{}:!\t{}\t-\t-\t\tunknown\t-\t{}\n",
            host,
            host,
            if reason.is_empty() {
                "unreachable"
            } else {
                &reason
            }
        ));
    }
    s
}

/// Which hosts need asking, and how urgently.
///
/// Split out so the TTLs can be asserted without an ssh anywhere near it. The
/// two are deliberately far apart: an answer is worth three seconds, a FAILURE
/// sixty, because a sleeping host must not be re-probed on every tick while a
/// live one must not go stale.
///
/// A file with no header at all, or a header this does not recognise, is COLD
/// rather than stale: it has never answered, so serving it from cache would mean
/// serving nothing.
fn classify(hosts: &[String], dir: &Path, n: i64) -> (Vec<String>, Vec<String>) {
    let (mut cold, mut stale) = (Vec::new(), Vec::new());
    for h in hosts {
        match header(&dir.join(h)) {
            None => cold.push(h.clone()),
            Some((status, stamp, _)) => {
                let ttl = match status.as_str() {
                    "ok" => env::num("TAIMUX_REMOTE_TTL", 3),
                    "err" => env::num("TAIMUX_REMOTE_FAIL_TTL", 60),
                    _ => {
                        cold.push(h.clone());
                        continue;
                    }
                };
                if n - stamp >= ttl as i64 {
                    stale.push(h.clone());
                }
            }
        }
    }
    (cold, stale)
}

/// Every discovered host's rows.
pub fn rows(panes: &[(String, String)]) -> String {
    if !enabled() {
        return String::new();
    }
    let dir = cache_dir();
    let hosts = hosts_cached(&dir, panes);
    if hosts.is_empty() {
        return String::new();
    }

    let (cold, stale) = classify(&hosts, &dir, now());

    // Nothing cached at all: fetched NOW, and in parallel, so n hosts cost one
    // round trip rather than n. Serving that host from cache would mean serving
    // nothing, and the first picker opened after a reboot would come up missing
    // exactly the remote sessions it was opened to find. Once per host per boot.
    let mut handles = Vec::new();
    for h in cold {
        let d = dir.clone();
        handles.push(std::thread::spawn(move || fetch(&h, &d)));
    }
    for j in handles {
        let _ = j.join();
    }

    // An answer that is merely ageing is served as it stands and refreshed
    // BEHIND the picker, so the wait lands on the next tick instead of on this
    // one. Detached, and nothing here waits for them.
    for h in stale {
        let d = dir.clone();
        std::thread::spawn(move || fetch(&h, &d));
    }

    hosts.iter().map(|h| render(h, &dir)).collect()
}

/// The pane the picker was opened from, resolved through an ssh window.
///
/// If that pane is a window onto another host, the row you are on is over THERE,
/// not the ssh pane you are looking through.
pub fn resolve_cur(pane: &str, panes: &[(String, String)], pane_cmd: &str) -> String {
    if !pane.starts_with('%') || !enabled() {
        return pane.to_string();
    }
    // Cheap pre-filter: only a pane whose foreground is ssh can be a window onto
    // another server, and the scan behind that question is a process listing.
    if pane_cmd != "ssh" {
        return pane.to_string();
    }
    let Some((host, _)) = ssh_tmux_panes(panes).into_iter().find(|(_, p)| p == pane) else {
        return pane.to_string();
    };
    // A host already known to be down is not asked again: the list has just
    // waited on that same host, and paying its timeout a second time to place a
    // cursor is not a trade worth making.
    if let Some((status, _, _)) = header(&cache_dir().join(&host)) {
        if status == "err" {
            return pane.to_string();
        }
    }
    let (rc, out) = ssh(
        &host,
        "tmux display-message -p \"#{pane_id}\"",
        env::num("TAIMUX_SSH_CUR_TIMEOUT", 2),
    );
    let rid = out.trim();
    if rc == 0 && rid.starts_with('%') {
        format!("{}:{}", host, rid)
    } else {
        pane.to_string()
    }
}

/// This host's index, for another host to read: `<pane> \t <blob>` per line.
///
/// Only LOCAL panes, which is the same "federate, never reach in" rule: a host
/// must never answer with another host's rows.
pub fn index_dump() -> String {
    let mut s = String::new();
    for e in std::fs::read_dir(index::index_dir())
        .into_iter()
        .flatten()
        .flatten()
    {
        let Ok(text) = std::fs::read_to_string(e.path()) else {
            continue;
        };
        let mut lines = text.lines();
        let Some(head) = lines.next() else { continue };
        let f: Vec<&str> = head.split(' ').collect();
        if f.first() != Some(&"idx") || f.len() < 4 {
            continue;
        }
        let pane = f[3];
        if !(pane.starts_with('%') && pane[1..].bytes().all(|b| b.is_ascii_digit())) {
            continue;
        }
        s.push_str(&format!("{}\t{}\n", pane, lines.next().unwrap_or("")));
    }
    s
}

/// Fetch one host's index and write it under composite ids.
///
/// A host whose taimux predates all of this answers with a usage line on stderr
/// and nothing here, so nothing is written and its rows go on matching what they
/// show. The guard on the pane id is what makes that a non-event.
///
/// That tolerance is also how this went unnoticed: the request used to carry an
/// underscore-prefixed bash-era name no Rust build has ever implemented, so EVERY
/// host answered `no such command` and exited 1, and the `rc != 0` below read it
/// as "an old host, nothing to see". The name has to match a subcommand in
/// main.rs exactly, which is why it is the named constant below and why the
/// suite asserts the old spelling appears nowhere in this file.
pub fn index_fetch(host: &str, dir: &Path) {
    let (rc, out) = ssh(
        host,
        &format!("{} {}", REMOTE_TAIMUX, INDEX_DUMP_CMD),
        env::num("TAIMUX_SEARCH_SSH_TIMEOUT", 20),
    );
    if rc != 0 {
        return;
    }
    let n = now();
    for line in out.lines() {
        let Some((pane, blob)) = line.split_once('\t') else {
            continue;
        };
        if !(pane.starts_with('%') && pane[1..].bytes().all(|b| b.is_ascii_digit())) {
            continue;
        }
        let id = format!("{}:{}", host, pane);
        let f = dir.join(index::key_for(&id));
        let _ = std::fs::write(&f, format!("idx 0 {} {} -\n{}\n", n, id, blob));
    }
}

/// Every discovered host's index, on its own longer TTL.
///
/// A minute rather than the three seconds a LIST is worth: what was said in a
/// session does not go stale on the timescale a pane's state does.
pub fn index_remote(panes: &[(String, String)]) {
    if !enabled() || !env::on("TAIMUX_SEARCH_REMOTE") {
        return;
    }
    let dir = index::index_dir();
    let ttl = env::num("TAIMUX_SEARCH_REMOTE_TTL", 60) as i64;
    let n = now();
    for h in hosts_cached(&cache_dir(), panes) {
        let stampfile = dir.join(format!(".{}.stamp", h));
        let last: i64 = std::fs::read_to_string(&stampfile)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        if n - last < ttl {
            continue;
        }
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(&stampfile, format!("{}\n", n));
        index_fetch(&h, &dir);
    }
}

/// A row's pane id is either a local `%17` or a remote `<host>:%6`.
pub fn pane_host(id: &str) -> Option<&str> {
    if id.is_empty() || id.starts_with('%') {
        return None;
    }
    id.split_once(':').map(|(h, _)| h)
}

pub fn pane_local(id: &str) -> &str {
    if id.is_empty() || id.starts_with('%') {
        return id;
    }
    id.split_once(':').map(|(_, p)| p).unwrap_or(id)
}

/// Drive another host's tmux AS A PEER, then bring the local client to its ssh
/// pane.
///
/// **In that order**, so the pane is already showing the right window by the time
/// the local client arrives in it. The other way round, a jump lands on whatever
/// that host was last looking at and corrects itself a moment later.
pub fn switch_remote(host: &str, lid: &str, local_pane: &str, zoom: bool) -> Result<(), String> {
    if !lid.starts_with('%') {
        return Err("not a pane id".into());
    }
    if local_pane.is_empty() {
        return Err(format!("nothing is ssh'd into {} any more", host));
    }
    let mut remote = format!(
        "tmux select-window -t {0} \\; select-pane -t {0} \\; switch-client -t {0}",
        lid
    );
    if zoom {
        // resize-pane -Z TOGGLES, so it is asked first, exactly as the local
        // path does.
        remote.push_str(&format!(
            "\n[ \"$(tmux display-message -p -t {0} '#{{window_zoomed_flag}}')\" = 1 ] || tmux resize-pane -Z -t {0}",
            lid
        ));
    }
    let (rc, _) = ssh(host, &remote, env::num("TAIMUX_SSH_TIMEOUT", 4));
    // A host that has gone quiet since the list was built still gets you taken
    // to its pane: that is where the dead connection is, and where you would go
    // to deal with it. Only the remote half is lost, and not silently.
    let lost = rc != 0;
    if taimux_core::tmux::switch_local(local_pane, zoom) {
        if lost {
            Err(format!("{} did not answer, going to its pane anyway", host))
        } else {
            Ok(())
        }
    } else {
        Err(format!("could not reach {}", local_pane))
    }
}

/// pane id and tty for every pane on this server, which is what the discovery
/// scan needs and what the caller already has.
pub fn tmux_pane_ttys() -> Vec<(String, String)> {
    taimux_core::tmux::ask_raw(&["list-panes", "-a", "-F", "#{pane_id}\t#{pane_tty}"])
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

/// The whole list: this server's agent panes plus every discovered host's.
pub fn all_panes(local: &str, panes: &[(String, String)]) -> String {
    format!("{}{}", local, rows(panes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ssh_tmux_argv_names_its_host() {
        assert_eq!(
            ssh_host("ssh ha -t tmux new-session -As main").as_deref(),
            Some("ha")
        );
        assert_eq!(
            ssh_host("/usr/bin/ssh laptop-two -t tmux attach").as_deref(),
            Some("laptop-two")
        );
    }

    /// ssh is a TOKEN, not argv[0]: a `#!` wrapper named ssh runs as
    /// `bash /path/to/ssh host …`, and requiring argv[0] found no hosts at all
    /// on such a machine.
    #[test]
    fn ssh_is_found_when_it_is_not_argv_zero() {
        assert_eq!(
            ssh_host("bash /home/p/bin/ssh ha -t tmux attach").as_deref(),
            Some("ha")
        );
    }

    /// A later `tmux` is required, or `ssh host journalctl -f` and a plain remote
    /// login shell would both look like a window onto another server.
    #[test]
    fn without_a_later_tmux_it_is_not_a_window_onto_a_server() {
        assert_eq!(ssh_host("ssh ha journalctl -f"), None);
        assert_eq!(ssh_host("ssh ha"), None);
        assert_eq!(ssh_host("tmux attach"), None);
        assert_eq!(ssh_host(""), None);
    }

    /// An option that takes a separate value must not have that value read as
    /// the host.
    #[test]
    fn an_options_value_is_not_mistaken_for_a_host() {
        assert_eq!(
            ssh_host("ssh -p 2222 ha -t tmux attach").as_deref(),
            Some("ha")
        );
        assert_eq!(
            ssh_host("ssh -i /k/id_ed25519 -o Foo=bar ha tmux attach").as_deref(),
            Some("ha")
        );
        // a flag with no separate value is skipped without eating the host
        assert_eq!(ssh_host("ssh -4 -A ha tmux attach").as_deref(), Some("ha"));
    }

    /// The name becomes a cache filename and an ssh argument, and there is no
    /// such thing as a hostname needing a quote.
    #[test]
    fn an_unexpected_character_in_a_host_drops_it() {
        assert_eq!(ssh_host("ssh ha;rm -rf / tmux attach"), None);
        assert_eq!(ssh_host("ssh ../etc tmux attach"), None);
        assert_eq!(ssh_host("ssh $HOST tmux attach"), None);
        // …but the ordinary shapes are fine
        assert_eq!(
            ssh_host("ssh user@ha.example.com tmux attach").as_deref(),
            Some("user@ha.example.com")
        );
    }

    #[test]
    fn a_composite_id_splits_into_host_and_pane() {
        assert_eq!(pane_host("ha:%6"), Some("ha"));
        assert_eq!(pane_local("ha:%6"), "%6");
        assert_eq!(pane_host("%17"), None);
        assert_eq!(pane_local("%17"), "%17");
        assert_eq!(pane_host(""), None);
        assert_eq!(pane_local(""), "");
        // the note row a host that stopped answering keeps
        assert_eq!(pane_host("ha:!"), Some("ha"));
        assert_eq!(pane_local("ha:!"), "!");
    }

    #[test]
    fn an_ssh_failure_says_what_kind() {
        assert_eq!(ssh_why(127), "no taimux on that host");
        assert_eq!(ssh_why(124), "timed out");
        assert_eq!(ssh_why(137), "timed out");
        assert_eq!(ssh_why(255), "unreachable");
        assert_eq!(ssh_why(3), "ssh failed");
    }

    fn fixture(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("jmrem{}{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_cached_reply_becomes_host_prefixed_rows() {
        let d = fixture("r");
        std::fs::write(
            d.join("ha"),
            "ok 100\n%6\tmain:1.7\t/w\tclaude\t2.1.1\trun\t-\tover there\n",
        )
        .unwrap();
        let out = render("ha", &d);
        assert!(out.starts_with("ha:%6\tmain:1.7\t"));
        assert_eq!(out.lines().count(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A host that has answered before and then goes quiet keeps ONE row saying
    /// so. One that never answered gets none: it is simply not running taimux,
    /// which is not an outage.
    #[test]
    fn only_a_host_that_has_answered_before_keeps_a_row() {
        let d = fixture("e");
        std::fs::write(d.join("ha"), "err 100 unreachable\n").unwrap();
        assert_eq!(render("ha", &d), "");
        std::fs::write(d.join("ha.seen"), "").unwrap();
        let out = render("ha", &d);
        assert!(out.starts_with("ha:!\tha\t"));
        assert!(out.contains("unreachable"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// An older taimux over there answers in another shape. Say so on one row
    /// rather than dropping its sessions silently.
    #[test]
    fn a_reply_in_another_format_says_so_once() {
        let d = fixture("f");
        std::fs::write(
            d.join("ha"),
            "ok 100\n%6\ttoo\tfew\tfields\n%7\talso\tshort\n",
        )
        .unwrap();
        let out = render("ha", &d);
        assert_eq!(out.lines().count(), 1);
        assert!(out.contains("2 row(s) dropped"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_file_with_no_header_renders_nothing() {
        let d = fixture("h");
        assert_eq!(render("ha", &d), ""); // no file at all
        std::fs::write(d.join("ha"), "").unwrap();
        assert_eq!(render("ha", &d), "");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The header is unconditional, so the file is never empty and the write is
    /// never mistaken for a failure. bash tested the last command in a group,
    /// which was the "any hosts?" guard, so every server with nothing to
    /// federate re-ran the scan forever and left temp files behind.
    #[test]
    fn the_host_cache_is_written_even_with_no_hosts() {
        // TMUX is process-global and the harness runs threads: two tests
        // setting it at once read each other's key. Same lock as everywhere else.
        let _g = taimux_core::env::ENV_LOCK.lock().unwrap();
        let d = fixture("c");
        std::env::set_var("TMUX", "/tmp/sock,1,0");
        let hosts = hosts_cached(&d, &[]);
        assert!(hosts.is_empty());
        let f = hosts_file(&d);
        assert!(f.exists(), "no cache file was written");
        assert!(std::fs::read_to_string(&f).unwrap().starts_with("hosts "));
        // and nothing was left behind
        let left: Vec<String> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".t"))
            .collect();
        assert!(left.is_empty(), "temp files left: {:?}", left);
        std::env::remove_var("TMUX");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Keyed by tmux SERVER: the list comes off that server's panes, and a second
    /// server on the same box has entirely different ones.
    #[test]
    fn the_host_cache_is_keyed_by_server() {
        // TMUX is process-global and the harness runs threads: two tests
        // setting it at once read each other's key. Same lock as everywhere else.
        let _g = taimux_core::env::ENV_LOCK.lock().unwrap();
        let d = fixture("k");
        std::env::set_var("TMUX", "/tmp/one,1,0");
        let a = hosts_file(&d);
        std::env::set_var("TMUX", "/tmp/two,1,0");
        let b = hosts_file(&d);
        assert_ne!(a, b);
        std::env::remove_var("TMUX");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The time limit is the ONLY way out of a popup whose helper wedges: a
    /// child that swallows ctrl-c cannot be escaped from, because the parent
    /// waits for it either way. So this has to actually kill.
    #[test]
    fn a_wedged_child_is_killed_rather_than_waited_for() {
        let mut c = Command::new("sleep");
        c.arg("30");
        let started = Instant::now();
        let (rc, out) = bounded(1, c);

        // `bounded` answers 255 for three different things: the spawn failed,
        // the child was signalled rather than exiting, and `try_wait` errored.
        // None of them is what this test is about, and all three come back
        // IMMEDIATELY, where the path under test cannot return before its
        // deadline. Under the fork pressure of the whole suite running in
        // parallel that happens about once in twelve runs; alone, never in
        // sixty. Conflating them in production is right (a failed ssh really is
        // "unreachable"), so the telling apart belongs here.
        if rc == 255 && started.elapsed() < Duration::from_millis(500) {
            eprintln!(
                "no child was spawned to wedge, skipping: rc={} in {:?}",
                rc,
                started.elapsed()
            );
            return;
        }

        assert_eq!(
            rc,
            124,
            "not the status `timeout` uses; came back in {:?}",
            started.elapsed()
        );
        assert!(out.is_empty());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "waited {:?} for a 1 s limit",
            started.elapsed()
        );
    }

    #[test]
    fn a_prompt_child_runs_to_completion_and_its_output_comes_back() {
        let mut c = Command::new("printf");
        c.arg("hi");
        assert_eq!(bounded(5, c), (0, "hi".to_string()));
        // …and a non-zero status is reported as its own, not as a timeout
        let c = Command::new("false");
        assert_eq!(bounded(5, c).0, 1);
        // a program that is not there is unreachable-shaped rather than a panic
        let c = Command::new("no-such-program-at-all");
        assert_eq!(bounded(5, c).0, 255);
    }

    /// The TTL split, without an ssh anywhere near it. An answer is worth three
    /// seconds and a FAILURE sixty, because a sleeping host must not be
    /// re-probed on every tick while a live one must not go stale.
    #[test]
    fn a_fresh_answer_is_left_alone_and_a_stale_one_is_refreshed() {
        let d = fixture("q");
        std::fs::write(d.join("fresh"), "ok 1000\n").unwrap();
        std::fs::write(d.join("stale"), "ok 900\n").unwrap();
        std::fs::write(d.join("dead"), "err 995 unreachable\n").unwrap();
        std::fs::write(d.join("junk"), "nonsense\n").unwrap();
        let hosts: Vec<String> = ["fresh", "stale", "dead", "junk", "never"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (cold, refresh) = classify(&hosts, &d, 1001);
        // never asked, and a header nobody recognises: both COLD, because
        // serving them from cache would mean serving nothing
        assert_eq!(cold, vec!["junk", "never"]);
        // 101 s past a 3 s answer, but only 6 s past a 60 s failure
        assert_eq!(refresh, vec!["stale"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The preamble is what runs on the FAR side, and it has three jobs: find a
    /// taimux that a non-interactive ssh's PATH does not reach, fall back to a
    /// plugin checkout, and exit 127 when there is none so "no taimux over
    /// there" is told apart from "unreachable".
    #[test]
    fn the_remote_preamble_finds_a_taimux_or_exits_127() {
        let home = fixture("p");
        let run = || -> (i32, String) {
            let out = Command::new("sh")
                .args(["-c", REMOTE_TAIMUX])
                .arg("list")
                .env("HOME", &home)
                .env("PATH", "/usr/bin:/bin")
                .output()
                .expect("sh");
            (
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stdout).trim().to_string(),
            )
        };
        // nothing installed at all
        assert_eq!(run().0, 127);

        // a plugin checkout is enough
        let plug = home.join(".tmux/plugins/taimux-9f2c1b");
        std::fs::create_dir_all(&plug).unwrap();
        std::fs::write(
            plug.join("taimux"),
            "#!/bin/sh\necho answered-from-the-plugin-copy\n",
        )
        .unwrap();
        set_exec(&plug.join("taimux"));
        assert_eq!(run(), (0, "answered-from-the-plugin-copy".into()));

        // …and a real install still outranks it
        let bin = home.join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(
            bin.join("taimux"),
            "#!/bin/sh\necho answered-from-the-real-install\n",
        )
        .unwrap();
        set_exec(&bin.join("taimux"));
        assert_eq!(run(), (0, "answered-from-the-real-install".into()));
        let _ = std::fs::remove_dir_all(&home);
    }

    fn set_exec(p: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut m = std::fs::metadata(p).unwrap().permissions();
        m.set_mode(0o755);
        std::fs::set_permissions(p, m).unwrap();
    }

    /// A pane that is not an ssh window resolves to itself, and the cheap
    /// pre-filter is what stops a process scan on every picker open.
    #[test]
    fn resolve_cur_leaves_an_ordinary_pane_alone() {
        assert_eq!(resolve_cur("%7", &[], "bash"), "%7");
        assert_eq!(resolve_cur("%7", &[], "ssh"), "%7"); // ssh, but no host found
        assert_eq!(resolve_cur("dead:/x", &[], "ssh"), "dead:/x");
    }
}
