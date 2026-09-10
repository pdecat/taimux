//! Every environment variable this tool reads, through one door.
//!
//! There was no such door before the rename: 38 call sites across 8 files each
//! reached for `std::env::var` with a literal, and one private `env_num` in
//! remote.rs did the numeric ones. That was survivable while the prefix never
//! changed. It stopped being survivable the moment it did, because a rename with
//! no choke point is a rename you cannot do gradually.
//!
//! So this module is not scaffolding. It stays after the transition below is
//! gone, because "which variables does this thing read, and what are the
//! defaults" is a question the code should be able to answer in one place.

/// A variable, by name.
///
/// An empty value is a value: `FOO=` is how a shell clears something it has
/// already exported, and it means "set to nothing", not "unset".
///
/// This wrapper looks thin, and briefly carried a fallback to the prefix the
/// variables had under two earlier names. That is gone, but the door stays: 38
/// call sites reaching for `std::env::var` with a literal is what made the first
/// rename impossible to do gradually, and one place that can answer "which
/// variables does this thing read" is worth a function call.
pub fn var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// A numeric variable, or `default` when it is unset or unparseable.
///
/// Unparseable takes the default rather than failing: these are all tuning
/// knobs, and a typo in one should not stop the picker opening.
pub fn num(name: &str, default: u64) -> u64 {
    var(name).and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Whether a variable is set to exactly `want`.
///
/// The two shapes in use are "on unless it says 0" and "off unless it says 1",
/// which read as `!is(x, "0")` and `is(x, "1")`. Spelling them out at the call
/// site was how one of them ended up inverted once.
pub fn is(name: &str, want: &str) -> bool {
    var(name).as_deref() == Some(want)
}

/// A boolean knob that is ON unless it is explicitly turned off with `0`.
pub fn on(name: &str) -> bool {
    !is(name, "0")
}

/// Tests that mutate the process environment have to take turns.
///
/// `set_var` is process-global and the test harness runs threads, so two of them
/// at once silently read each other's fixture. That is exactly what happened
/// once: adding a third such test broke two existing ones, and neither failure
/// named the cause.
///
/// It lives here because this is the module about the environment, and because
/// its previous home made the dependency graph a ring: conv owned it, indexer
/// needs conv, and this module needs indexer's neighbours. Anything that reaches
/// for `set_var` in a test takes this.
///
/// **Public, and not `#[cfg(test)]`, on purpose.** `cli` and `daemon` have tests
/// that mutate the environment too, and when their test binaries compile they
/// link `core` as an ordinary dependency, where a `cfg(test)` item does not
/// exist and a `pub(crate)` one is not visible. `#[doc(hidden)]` keeps it out of
/// the documented surface; the cost of it existing in a release build is one
/// unlocked mutex.
///
/// One lock per test binary is the right amount rather than a compromise. Each
/// binary is its own process with its own environment, so there is nothing to
/// serialise ACROSS them: the races this prevents are always between threads
/// sharing one process's `environ`.
#[doc(hidden)]
pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    // An exported-but-empty value is what a shell leaves behind when it clears
    // a variable it had set, and it is NOT the same as unset.
    #[test]
    fn an_empty_value_is_a_value() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("TAIMUX_TESTEMPTY", "");
        assert_eq!(var("TAIMUX_TESTEMPTY").as_deref(), Some(""));
        std::env::remove_var("TAIMUX_TESTEMPTY");
        assert_eq!(var("TAIMUX_TESTEMPTY"), None);
    }

    // An exported-but-empty value is what a shell leaves behind when it clears a
    // variable it had set, and it must NOT fall through to the old name: the
    // caller asked for the new one and got an answer.
    #[test]
    fn unset_everywhere_is_none() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("TAIMUX_TESTKNOB4");
        assert_eq!(var("TAIMUX_TESTKNOB4"), None);
    }

    // A name that is not ours at all has no legacy spelling to try, and must not
    // be rewritten into one.
    #[test]
    fn num_takes_the_default_when_unset_or_junk() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("TAIMUX_TESTNUM");
        assert_eq!(num("TAIMUX_TESTNUM", 7), 7);
        std::env::set_var("TAIMUX_TESTNUM", "not a number");
        assert_eq!(num("TAIMUX_TESTNUM", 7), 7);
        std::env::set_var("TAIMUX_TESTNUM", "12");
        assert_eq!(num("TAIMUX_TESTNUM", 7), 12);
        std::env::remove_var("TAIMUX_TESTNUM");
    }

    #[test]
    fn on_is_true_unless_zero() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("TAIMUX_TESTON");
        assert!(on("TAIMUX_TESTON"));
        std::env::set_var("TAIMUX_TESTON", "0");
        assert!(!on("TAIMUX_TESTON"));
        std::env::set_var("TAIMUX_TESTON", "1");
        assert!(on("TAIMUX_TESTON"));
        std::env::remove_var("TAIMUX_TESTON");
    }
}
