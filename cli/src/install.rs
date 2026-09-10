//! Putting taimux on PATH and binding the key.
//!
//! Step 6 of replacing bash, and the last of it. This is the part that writes to
//! a real configuration file, so the rules it follows are about not making a mess
//! of somebody's tmux config:
//!
//! - **Idempotent by the command string.** A past install's block is stripped
//!   from every file it could have touched before a new one is written, or a
//!   reinstall registers the binding twice and the popup opens twice.
//! - **Oh My Tmux gets `.tmux.conf.local`**, never `.tmux.conf`. That file is
//!   both configuration AND a shell script (its `_apply_configuration` is run
//!   through `cut | sh`), so appending raw config to the wrong one breaks every
//!   reload.
//! - **Inside the "user customizations" section** where there is one, because in
//!   Oh My Tmux that section sits inside a heredoc and is therefore safe from
//!   that pass.
//! - **A plugin-manager checkout is left alone entirely.** There the plugin
//!   entry point binds the keys on every launch and reload, so writing to the
//!   config would be a second, stale copy of the same binding.
//! - **The bindings call the bare launcher name**, never the checkout path, so
//!   moving the checkout does not break them.

use std::io::Write;
use std::path::{Path, PathBuf};

use taimux_core::tmux;

/// tmux's `#{<:}` is a string compare, so the numeric width test is done in the
/// shell, evaluated against the triggering client's width at key-press time.
const POPUP_COND: &str = "[ \"#{client_width}\" -lt 100 ]";

/// How much of the client a popup takes: near-full on a small screen (a phone
/// over ssh), 80% on a roomy terminal.
///
/// One rule, two callers. The binding below opens the popup with it at
/// key-press time, and the picker reopens itself with it when the terminal has
/// grown since (see `tui::outgrown`), because tmux never grows a popup past the
/// size it was asked for. Two copies of this would drift into a picker that
/// resizes itself to a geometry the binding would never have chosen.
pub fn popup_geometry(client_width: usize) -> (u16, u16) {
    if client_width < 100 {
        (100, 90)
    } else {
        (80, 80)
    }
}

/// `#{pane_id}` in there is inert on purpose: `display-popup` expands no format
/// in the command it runs, so bare like this the shell drops it as a comment and
/// the picker asks tmux which pane it was opened from. Quoting it would hand the
/// picker the format itself as a string.
///
/// `-e TAIMUX_POPUP=1` is how the picker knows it is IN a popup rather than
/// inline in a pane, which decides whether it may reopen itself on a resize. It
/// is set here rather than guessed over there because every guess available (our
/// size against the active pane's, a leaked `$TMUX_PANE`) is wrong for some real
/// layout, and guessing wrong means an inline picker closing itself and coming
/// back as a popup.
pub fn popup_cmd(launcher: &str, small: bool) -> String {
    let (w, h) = popup_geometry(if small { 80 } else { 100 });
    format!(
        "display-popup -E -e TAIMUX_POPUP=1 -w {}% -h {}% \"{} pick #{{pane_id}}\"",
        w, h, launcher
    )
}

/// A tmux user option, or a default when it is not SET at all.
///
/// Set-but-empty and unset are different answers: an empty `@taimux-key` means
/// "do not bind that one", which is why this asks whether the option exists
/// before reading its value.
pub fn tmux_opt(name: &str, default: &str) -> String {
    match tmux::ask(&["show-options", "-gq", name]) {
        Some(shown) if !shown.trim().is_empty() => {
            tmux::ask(&["show-options", "-gqv", name]).unwrap_or_default()
        }
        _ => default.to_string(),
    }
}

/// Bind the keys in the RUNNING server, and say what got bound.
pub fn bind_live(launcher: &str, key: &str, root: &str) -> String {
    let (small, large) = (popup_cmd(launcher, true), popup_cmd(launcher, false));
    let mut desc = String::new();
    if !key.is_empty() && tmux::run(&["bind-key", key, "if-shell", POPUP_COND, &small, &large]) {
        desc = format!("prefix + {}", key);
    }
    if !root.is_empty()
        && tmux::run(&[
            "bind-key", "-n", root, "if-shell", POPUP_COND, &small, &large,
        ])
    {
        if desc.is_empty() {
            desc = root.to_string();
        } else {
            desc = format!("{} and {}", desc, root);
        }
    }
    desc
}

/// Is this checkout owned by a plugin manager?
///
/// If it is, the plugin entry point binds the keys on every launch and reload, so
/// `install` must not also write them to a config file.
pub fn plugin_checkout(exe: &Path) -> bool {
    let Some(parent) = exe.parent().and_then(|p| p.parent()) else {
        return false;
    };
    let home = std::env::var("HOME").unwrap_or_default();
    let xdg = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{}/.config", home));
    [
        std::env::var("TMUX_PLUGIN_MANAGER_PATH").unwrap_or_default(),
        format!("{}/.tmux/plugins", home),
        format!("{}/tmux/plugins", xdg),
    ]
    .iter()
    .any(|p| !p.is_empty() && parent == Path::new(p.trim_end_matches('/')))
}

/// Is this an Oh My Tmux configuration?
///
/// Two tells, and both are needed: the resolved path living under `~/.tmux/`, and
/// the `_apply_configuration` function that makes that file a shell script as
/// well as a config.
pub fn is_oh_my_tmux(resolved: &Path) -> bool {
    let home = std::env::var("HOME").unwrap_or_default();
    if resolved.starts_with(format!("{}/.tmux/", home)) {
        return true;
    }
    std::fs::read_to_string(resolved)
        .map(|t| t.contains("_apply_configuration"))
        .unwrap_or(false)
}

/// Which file to write to.
///
/// `.tmux.conf.local` whenever it exists OR the config is Oh My Tmux, because
/// that is the file Oh My Tmux reserves for exactly this and the only one it is
/// safe to append to.
pub fn choose_conf(resolved: &Path, local: &Path) -> PathBuf {
    if local.exists() || is_oh_my_tmux(resolved) {
        local.to_path_buf()
    } else {
        resolved.to_path_buf()
    }
}

/// Remove any block a past install added, under any of this tool's former names.
///
/// A file with nothing of ours in it is left completely untouched, mtime
/// included: a reinstall must not look like an edit to something watching the
/// file.
pub fn strip_block(text: &str) -> Option<String> {
    // Every name this tool has installed under. A config edited years ago still
    // has one of them in it, and leaving it would bind the popup twice.
    let ours = |l: &str| {
        l.contains("claude-tmux-locator pick")
            || l.contains("muxhop pick")
            || l.contains("taimux pick")
            || l.starts_with("# claude-tmux-locator")
            || l.starts_with("#claude-tmux-locator")
            || l.starts_with("# muxhop")
            || l.starts_with("#muxhop")
            || l.starts_with("# taimux")
            || l.starts_with("#taimux")
    };
    if !text.lines().any(ours) {
        return None;
    }
    Some(
        text.lines()
            .filter(|l| !ours(l))
            .map(|l| format!("{}\n", l))
            .collect(),
    )
}

/// Insert the block, under the "user customizations" heading where there is one.
///
/// In Oh My Tmux that heading sits inside a heredoc, which is what makes the
/// insertion safe from the `cut | sh` pass that file gets put through. Appended
/// at the end otherwise.
pub fn insert_block(text: &str, block: &str) -> String {
    let mut out = String::new();
    let mut done = false;
    for l in text.lines() {
        out.push_str(l);
        out.push('\n');
        if !done && l.contains("-- user customizations") {
            out.push_str(block);
            done = true;
        }
    }
    if !done {
        out.push_str(block);
    }
    out
}

/// The block itself: a marker line saying what it is, then one bind per key.
pub fn block_for(launcher: &str, key: &str, root: &str, desc: &str) -> String {
    let (small, large) = (popup_cmd(launcher, true), popup_cmd(launcher, false));
    let mut b = format!(
        "\n# taimux - jump between agent sessions - {} (adaptive popup width)\n",
        if desc.is_empty() { "unbound" } else { desc }
    );
    if !key.is_empty() {
        b.push_str(&format!(
            "bind-key {} if-shell '{}' '{}' '{}'\n",
            key, POPUP_COND, small, large
        ));
    }
    if !root.is_empty() {
        // no prefix, so it is easy to send from a phone ssh app
        b.push_str(&format!(
            "bind-key -n {} if-shell '{}' '{}' '{}'\n",
            root, POPUP_COND, small, large
        ));
    }
    b
}

/// How the two keys read in a sentence.
pub fn describe(key: &str, root: &str) -> String {
    match (key.is_empty(), root.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!("prefix + {}", key),
        (true, false) => root.to_string(),
        (false, false) => format!("prefix + {} and {}", key, root),
    }
}

/// `taimux bind`: bind the keys in the running server and write nothing.
///
/// This is what the tmux plugin entry point calls on every launch and reload, and
/// it is also the repair for a binding whose checkout has moved.
pub fn bind(exe: &str) -> i32 {
    if tmux::ask(&["show-options", "-g"]).is_none() {
        eprintln!(
            "no tmux server to bind in: run it from inside tmux, or let your plugin manager run it"
        );
        return 1;
    }
    let key = tmux_opt("@taimux-key", "a");
    let root = tmux_opt("@taimux-root-key", "F1");
    let desc = bind_live(exe, &key, &root);
    if desc.is_empty() {
        println!("nothing bound: @taimux-key and @taimux-root-key are both set to empty");
        return 0;
    }
    println!("bound {} -> {}", desc, exe);
    0
}

/// `taimux install`: symlink the launcher and bind the key.
pub fn install(exe: &str) -> i32 {
    let home = std::env::var("HOME").unwrap_or_default();
    let bindir = PathBuf::from(&home).join(".local/bin");
    let link = bindir.join("taimux");
    let _ = std::fs::create_dir_all(&bindir);
    let _ = std::fs::remove_file(&link);
    if std::os::unix::fs::symlink(exe, &link).is_err() {
        eprintln!("could not symlink {}", link.display());
        return 1;
    }
    println!("symlinked {} -> {}", link.display(), exe);

    // Drop stale symlinks from earlier names of this tool, if they are ours.
    //
    // "ours" is judged by what the link POINTS AT, never by the name alone:
    // `cj` is two letters and could easily be somebody else's.
    for old in ["cj", "muxhop"] {
        let p = bindir.join(old);
        if let Ok(t) = std::fs::read_link(&p) {
            let t = t.to_string_lossy();
            if t.contains("claude-tmux-locator") || t.ends_with("/muxhop") || t.ends_with("/taimux")
            {
                let _ = std::fs::remove_file(&p);
                println!("removed old `{}` symlink", old);
            }
        }
    }

    let key = tmux_opt("@taimux-key", "a");
    let root = tmux_opt("@taimux-root-key", "F1");
    let desc = describe(&key, &root);
    let in_tmux = std::env::var("TMUX")
        .map(|v| !v.is_empty())
        .unwrap_or(false);

    // A checkout a plugin manager owns gets its bindings from the plugin entry
    // point on every launch and reload, so the only half of `install` left to do
    // is the symlink: that is what puts restart, resurrect and install-hooks on
    // PATH, and what answers ssh when this host is listed from another one.
    if plugin_checkout(Path::new(exe)) {
        println!("plugin checkout, so your plugin manager owns the bindings: nothing");
        println!("written to your tmux config.");
        if in_tmux && !bind_live(exe, &key, &root).is_empty() {
            println!("bound {} in the running tmux server", desc);
        }
        println!("\nDone. Optional: taimux install-hooks, so sessions report their own state.");
        return 0;
    }

    let resolved = std::fs::canonicalize(PathBuf::from(&home).join(".tmux.conf"))
        .unwrap_or_else(|_| PathBuf::from(&home).join(".tmux.conf"));
    let local = PathBuf::from(&home).join(".tmux.conf.local");
    let target = choose_conf(&resolved, &local);
    if !target.exists() {
        let _ = std::fs::write(&target, "");
    }
    println!("writing bindings to {}", target.display());

    // Idempotent: strip any prior block from every file a past install may have
    // touched, not just the one being written now.
    for f in [&resolved, &local] {
        if let Ok(text) = std::fs::read_to_string(f) {
            if let Some(stripped) = strip_block(&text) {
                let _ = std::fs::write(f, stripped);
            }
        }
    }

    let text = std::fs::read_to_string(&target).unwrap_or_default();
    // The bare launcher name, never the checkout path, so moving the checkout
    // does not break the binding.
    let body = insert_block(&text, &block_for("taimux", &key, &root, &desc));
    if std::fs::write(&target, body).is_err() {
        eprintln!("could not write {}", target.display());
        return 1;
    }
    println!(
        "bound {} in {}",
        if desc.is_empty() { "nothing" } else { &desc },
        target.display()
    );

    if in_tmux && !bind_live("taimux", &key, &root).is_empty() {
        println!("bound {} in the running tmux server", desc);
    }
    if desc.is_empty() {
        println!("\nDone. Both key options are set to empty, so no key is bound; run: taimux");
    } else {
        println!("\nDone. Press {}, or run: taimux", desc);
    }
    println!("Optional: taimux install-hooks, so sessions report their own state.");
    let _ = std::io::stdout().flush();
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_popup_is_wider_on_a_narrow_client() {
        assert!(popup_cmd("taimux", true).contains("-w 100%"));
        assert!(popup_cmd("taimux", false).contains("-w 80%"));
        // the pane id goes in BARE: display-popup expands no format, so the shell
        // drops it as a comment and the picker asks tmux instead
        assert!(popup_cmd("taimux", false).contains("pick #{pane_id}"));
    }

    #[test]
    fn the_keys_read_as_a_sentence() {
        assert_eq!(describe("a", "F1"), "prefix + a and F1");
        assert_eq!(describe("a", ""), "prefix + a");
        assert_eq!(describe("", "F1"), "F1");
        assert_eq!(describe("", ""), "");
    }

    /// A file with nothing of ours in it is left completely untouched, mtime
    /// included: a reinstall must not look like an edit to anything watching it.
    #[test]
    fn a_file_without_our_block_is_not_rewritten() {
        assert_eq!(strip_block("set -g mouse on\n"), None);
    }

    /// Every former name of this tool, because a config edited years ago still
    /// has one of them in it and leaving it would bind the popup twice.
    #[test]
    fn a_past_block_is_stripped_under_any_of_the_old_names() {
        let text = "set -g mouse on\n\
                    # taimux - jump between agent sessions\n\
                    bind-key a if-shell '…' 'display-popup -E … \"taimux pick\"' '…'\n\
                    # muxhop\n\
                    bind-key -n F1 run 'muxhop pick'\n\
                    bind-key X run 'claude-tmux-locator pick'\n\
                    set -g status on\n";
        let out = strip_block(text).expect("stripped");
        assert_eq!(out, "set -g mouse on\nset -g status on\n");
    }

    /// In Oh My Tmux the "user customizations" heading sits inside a heredoc,
    /// which is what makes an insertion there safe from the `cut | sh` pass that
    /// file gets put through.
    #[test]
    fn the_block_goes_under_the_user_customizations_heading() {
        let text = "# -- user customizations\n# after\n";
        let out = insert_block(text, "\nBLOCK\n");
        assert_eq!(out, "# -- user customizations\n\nBLOCK\n# after\n");
    }

    #[test]
    fn without_that_heading_it_goes_at_the_end() {
        let out = insert_block("set -g mouse on\n", "\nBLOCK\n");
        assert_eq!(out, "set -g mouse on\n\nBLOCK\n");
    }

    /// Only the FIRST heading, or a config mentioning it twice gets two blocks.
    #[test]
    fn only_the_first_heading_takes_the_block() {
        let text = "# -- user customizations\nx\n# -- user customizations\n";
        let out = insert_block(text, "\nBLOCK\n");
        assert_eq!(out.matches("BLOCK").count(), 1);
    }

    /// Oh My Tmux reserves `.tmux.conf.local` for exactly this, and appending to
    /// `.tmux.conf` there breaks every reload, since that file is a shell script
    /// as well as a config.
    #[test]
    fn oh_my_tmux_gets_the_local_file() {
        // HOME is process-global and the harness runs threads.
        let _g = taimux_core::env::ENV_LOCK.lock().unwrap();
        let d = std::env::temp_dir().join(format!("jminst{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let conf = d.join("tmux.conf");
        let local = d.join("tmux.conf.local");

        // a plain config, no local file: write to the config itself
        std::fs::write(&conf, "set -g mouse on\n").unwrap();
        assert_eq!(choose_conf(&conf, &local), conf);

        // Oh My Tmux, detected by its own function: the local file, even though
        // it does not exist yet
        std::fs::write(&conf, "_apply_configuration() {\n  :\n}\n").unwrap();
        assert_eq!(choose_conf(&conf, &local), local);

        // and a local file that exists always wins
        std::fs::write(&conf, "set -g mouse on\n").unwrap();
        std::fs::write(&local, "").unwrap();
        assert_eq!(choose_conf(&conf, &local), local);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A plugin-manager checkout must not have bindings written to a config file:
    /// the plugin entry point binds them on every launch, and a written copy
    /// would be a second, stale one.
    #[test]
    fn a_plugin_checkout_is_recognised() {
        // HOME is process-global and the harness runs threads.
        let _g = taimux_core::env::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!("jmplug{}", std::process::id()));
        std::env::set_var("HOME", &home);
        std::env::remove_var("TMUX_PLUGIN_MANAGER_PATH");
        std::env::remove_var("XDG_CONFIG_HOME");
        let inside = home.join(".tmux/plugins/taimux/taimux");
        assert!(plugin_checkout(&inside));
        let outside = home.join("workspaces/ai/taimux/taimux");
        assert!(!plugin_checkout(&outside));
        // …and the manager's own path, wherever it points
        std::env::set_var("TMUX_PLUGIN_MANAGER_PATH", home.join("elsewhere"));
        assert!(plugin_checkout(&home.join("elsewhere/taimux/taimux")));
        std::env::remove_var("TMUX_PLUGIN_MANAGER_PATH");
        std::env::remove_var("HOME");
        let _ = std::fs::remove_dir_all(&home);
    }
}

/// Register the hook for the five turn boundaries.
///
/// **Through `jq`, deliberately**, and this is the one place a fork is the right
/// answer rather than a leftover. `settings.json` is a file the AGENT rewrites
/// for itself, so anything that mangles it is a config lost, and hand-rolled JSON
/// editing over somebody's real configuration is exactly how that happens. jq
/// gets the merge right, preserves everything it does not touch, and refuses an
/// unparseable file rather than replacing it.
///
/// The command string is what makes the entry idempotent, so it has to be spelled
/// the same way every time: a config managed from somewhere else (chezmoi) that
/// spells it differently would otherwise end up with the hook registered twice,
/// firing twice per event.
pub fn install_hooks(exe: &str) -> i32 {
    const EVENTS: &str = "SessionStart UserPromptSubmit Stop PermissionRequest SessionEnd";
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = std::env::var("CLAUDE_CONFIG_DIR").unwrap_or_else(|_| format!("{}/.claude", home));
    let settings = PathBuf::from(&dir).join("settings.json");
    let cmd = format!("{} hook", exe);

    if which("jq").is_none() {
        println!("jq not found, so settings.json is left alone. Add this by hand:\n");
        println!("  command: {}\n  events:  {}\n", cmd, EVENTS);
        return 1;
    }
    let _ = std::fs::create_dir_all(&dir);
    if !settings.exists() {
        let _ = std::fs::write(&settings, "{}\n");
    }
    let prog = r#"
        def ensure($event):
          .hooks //= {}
          | .hooks[$event] //= []
          | if [.hooks[$event][]?.hooks[]?.command] | index($cmd) then .
            else .hooks[$event] += [{hooks: [{type: "command", command: $cmd}]}]
            end;
        reduce ($events | split(" ")[]) as $e (.; ensure($e))
    "#;
    let out = std::process::Command::new("jq")
        .args([
            "--indent", "2", "--arg", "cmd", &cmd, "--arg", "events", EVENTS, prog,
        ])
        .arg(&settings)
        .output();
    let Ok(out) = out else {
        eprintln!("could not run jq");
        return 1;
    };
    // An unparseable settings file is left exactly as it was. It is not ours to
    // repair, and replacing it would lose whatever is in there.
    if !out.status.success() || out.stdout.is_empty() {
        println!(
            "{} is not valid JSON, so it was left alone.",
            settings.display()
        );
        return 1;
    }
    let tmp = settings.with_extension(format!("taimux.{}", std::process::id()));
    if std::fs::write(&tmp, &out.stdout).is_err() || std::fs::rename(&tmp, &settings).is_err() {
        let _ = std::fs::remove_file(&tmp);
        eprintln!("could not write {}", settings.display());
        return 1;
    }
    println!(
        "registered `{}` for {} in {}",
        cmd,
        EVENTS,
        settings.display()
    );
    println!("Sessions already running keep reporting nothing until they restart.");
    0
}

/// Is a program on PATH? Our own, so the check costs no fork.
fn which(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    for d in std::env::var("PATH").unwrap_or_default().split(':') {
        if d.is_empty() {
            continue;
        }
        let p = Path::new(d).join(name);
        if p.is_file()
            && std::fs::metadata(&p)
                .map(|m| m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod which_tests {
    use super::*;

    /// A file that is there but NOT executable is not a program, which is the
    /// difference between "jq is missing" and "jq is broken".
    #[test]
    fn an_executable_on_path_is_found_and_a_plain_file_is_not() {
        assert!(which("sh").is_some());
        assert!(which("no-such-program-anywhere").is_none());
        let d = std::env::temp_dir().join(format!("jmwhich{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("notexec"), "x").unwrap();
        let _g = taimux_core::env::ENV_LOCK.lock().unwrap();
        let old = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", d.to_string_lossy().as_ref());
        assert!(which("notexec").is_none());
        std::env::set_var("PATH", old);
        let _ = std::fs::remove_dir_all(&d);
    }
}
