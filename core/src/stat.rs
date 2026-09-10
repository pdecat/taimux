//! What a refresh spent its time on, counted where it is spent.
//!
//! Exists because a freeze was reported that nothing here could explain
//! afterwards: the picker sat on a sweep's last screen for the whole 85 seconds
//! its restarts took, and every part measured in isolation came back in
//! milliseconds (the pane scan 0.09s over 171 panes, a tmux capture 2ms, a popup
//! round trip 7ms, the sweep child 0.77s end to end). A slow refresh leaves no
//! trace of its own, so the next one would have been just as unexplainable.
//!
//! Counters rather than a stats struct threaded through six signatures: the
//! three things that fork (a capture, a version probe, an ssh) are in three
//! different modules, and the alternative is a parameter on every function
//! between here and them. They are process-global and the picker is one refresh
//! at a time, so `reset` then `report` brackets exactly one.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

macro_rules! counters {
    ($($count:ident, $micros:ident, $label:literal;)*) => {
        $(
            static $count: AtomicU64 = AtomicU64::new(0);
            static $micros: AtomicU64 = AtomicU64::new(0);
        )*

        /// Start a fresh measurement.
        pub fn reset() {
            $(
                $count.store(0, Ordering::Relaxed);
                $micros.store(0, Ordering::Relaxed);
            )*
        }

        /// What has been counted since, as one line, quietest parts omitted: a
        /// report is read when something took too long, so a zero says nothing.
        pub fn report() -> String {
            let mut parts: Vec<String> = Vec::new();
            $(
                let n = $count.load(Ordering::Relaxed);
                if n > 0 {
                    parts.push(format!(
                        "{} {} in {:.1}s",
                        n,
                        $label,
                        $micros.load(Ordering::Relaxed) as f64 / 1e6
                    ));
                }
            )*
            parts.join(", ")
        }
    };
}

counters! {
    CAPTURES, CAPTURE_US, "captures";
    PROBES, PROBE_US, "version probes";
    SSH, SSH_US, "ssh calls";
}

/// Time one call and count it. Returns whatever the call returned, so it wraps
/// an existing expression rather than needing a place to put a timer.
fn timed<T>(count: &AtomicU64, micros: &AtomicU64, f: impl FnOnce() -> T) -> T {
    let at = Instant::now();
    let out = f();
    count.fetch_add(1, Ordering::Relaxed);
    micros.fetch_add(at.elapsed().as_micros() as u64, Ordering::Relaxed);
    out
}

pub fn capture<T>(f: impl FnOnce() -> T) -> T {
    timed(&CAPTURES, &CAPTURE_US, f)
}

pub fn probe<T>(f: impl FnOnce() -> T) -> T {
    timed(&PROBES, &PROBE_US, f)
}

pub fn ssh<T>(f: impl FnOnce() -> T) -> T {
    timed(&SSH, &SSH_US, f)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The report names only what actually happened, so a slow refresh line is
    /// about the thing that was slow rather than a row of zeroes to read past.
    #[test]
    fn the_report_omits_what_never_ran() {
        reset();
        assert_eq!(report(), "");
        capture(|| ());
        capture(|| ());
        let r = report();
        assert!(r.starts_with("2 captures in "), "got {r:?}");
        assert!(!r.contains("ssh"), "got {r:?}");
    }

    #[test]
    fn every_kind_is_counted_and_timed() {
        reset();
        probe(|| ());
        ssh(|| ());
        let r = report();
        assert!(r.contains("1 version probes in "), "got {r:?}");
        assert!(r.contains("1 ssh calls in "), "got {r:?}");
    }

    /// A reset has to clear the clock as well as the count, or the next refresh
    /// inherits the last one's seconds and every report after a slow one lies.
    #[test]
    fn a_reset_clears_the_clock_too() {
        reset();
        capture(|| std::thread::sleep(std::time::Duration::from_millis(5)));
        reset();
        capture(|| ());
        let r = report();
        assert!(r.starts_with("1 captures in 0.0s"), "got {r:?}");
    }
}
