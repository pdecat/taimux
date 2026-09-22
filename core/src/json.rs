//! Pulling one string field out of a JSON line, without a JSON parser.
//!
//! There is no serde here and there does not need to be. Everything read this
//! way is one line of a transcript or one hook payload, the wanted fields are
//! flat strings, and a dependency that pulls in a derive macro and a parser to
//! find `"cwd"` would be the largest thing in the crate.
//!
//! This module exists because there were **three** copies of it: `hook::json_str`,
//! `indexer::val` (behind `indexer::json_field`) and `version::json_string_field`.
//! Three copies is how `conv` came to depend on `indexer` for a twenty-line
//! string scan, which is a cycle rather than a dependency, and the reason the
//! crate could not be split.
//!
//! **The two behaviours below are deliberately still two.** They differ, and the
//! difference is load-bearing on real data, so folding them into one function
//! would be a silent behaviour change on the transcript path dressed up as a
//! cleanup. Unifying them is a decision with its own test burden, not a
//! side-effect of moving code.

/// The first `"key": "value"` in the text, or None.
///
/// Gives up as soon as the first occurrence of the key is not followed by a
/// string value. That is the right call for the shapes it reads, where the key
/// appears once and a miss means the field is genuinely not there.
pub fn first(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\"", key);
    let start = text.find(&needle)? + needle.len();
    let rest = text[start..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// The first `"key": "value"` in the text, **still looking** past a match that
/// is not one.
///
/// The difference from `first` matters on hook payloads, where the key can
/// legitimately appear earlier inside somebody else's string value: a prompt
/// quoting `"permission_mode"` back at us, or a nested object listing it as a
/// value rather than a key. `first` would read that as "not present" and stop;
/// this keeps going and finds the real field.
///
/// It is not simply better. Scanning on means a key genuinely absent is looked
/// for to the end of the payload, and it can match a later occurrence that
/// `first` would never have reached, so the two are not interchangeable.
pub fn scan(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\"", key);
    let mut from = 0;
    while let Some(at) = text[from..].find(&needle) {
        let after = &text[from + at + needle.len()..];
        let trimmed = after.trim_start();
        if let Some(rest) = trimmed.strip_prefix(':') {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('"') {
                if let Some(end) = rest.find('"') {
                    return Some(rest[..end].to_string());
                }
            }
        }
        from += at + needle.len();
    }
    None
}

/// Whether `"key": [ ... ]` holds anything, looking past decoys the way `scan`
/// does. `None` when the key is not there as an array at all.
///
/// All a hook needs to know about `Stop`'s `background_tasks`: an empty list
/// means the session is done, anything in it means work is still in flight that
/// will wake the session again. What the entries say is not read.
pub fn array_has_items(text: &str, key: &str) -> Option<bool> {
    let needle = format!("\"{}\"", key);
    let mut from = 0;
    while let Some(at) = text[from..].find(&needle) {
        let after = &text[from + at + needle.len()..];
        if let Some(rest) = after.trim_start().strip_prefix(':') {
            if let Some(rest) = rest.trim_start().strip_prefix('[') {
                return Some(!rest.trim_start().starts_with(']'));
            }
        }
        from += at + needle.len();
    }
    None
}

/// `first`, with an absent field as the empty string.
///
/// The transcript readers want a `String` they can compare against a literal,
/// and threading `Option` through them buys nothing: an absent field and an
/// empty one mean the same thing to every one of those call sites.
pub fn field(line: &str, key: &str) -> String {
    first(line, key).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_field() {
        assert_eq!(first(r#"{"a":"b"}"#, "a").as_deref(), Some("b"));
        assert_eq!(scan(r#"{"a":"b"}"#, "a").as_deref(), Some("b"));
        assert_eq!(field(r#"{"a":"b"}"#, "a"), "b");
    }

    #[test]
    fn whitespace_around_the_colon_is_allowed() {
        let t = "{\n  \"version\"  :   \"1.2.3\"\n}";
        assert_eq!(first(t, "version").as_deref(), Some("1.2.3"));
        assert_eq!(scan(t, "version").as_deref(), Some("1.2.3"));
    }

    #[test]
    fn an_absent_field() {
        assert_eq!(first("{}", "version"), None);
        assert_eq!(scan("{}", "version"), None);
        assert_eq!(field("{}", "version"), "");
    }

    /// The one case that separates them, and the reason both survive the merge.
    ///
    /// A decoy only counts if it is the key **exactly as quoted**, `"key"`, since
    /// that is the needle. Prose mentioning the bare word does not divert either
    /// function, and neither does an escaped `\"key\"`, because the backslash
    /// sits where the closing quote would have to be and the needle simply does
    /// not match there. Both are asserted, because both look like they should
    /// diverge and neither does.
    #[test]
    fn only_scan_looks_past_a_false_first_hit() {
        let prose = r#"{"prompt":"what is permission_mode for","permission_mode":"plan"}"#;
        assert_eq!(scan(prose, "permission_mode").as_deref(), Some("plan"));
        assert_eq!(first(prose, "permission_mode").as_deref(), Some("plan"));

        let escaped = r#"{"note":"\"permission_mode\" is unset","permission_mode":"plan"}"#;
        assert_eq!(scan(escaped, "permission_mode").as_deref(), Some("plan"));
        assert_eq!(first(escaped, "permission_mode").as_deref(), Some("plan"));

        // A quoted token that is NOT a key: the needle matches, and what follows
        // is `]` rather than a colon. THIS is where they part company.
        let listed = r#"{"fields":["permission_mode"],"permission_mode":"plan"}"#;
        assert_eq!(scan(listed, "permission_mode").as_deref(), Some("plan"));
        assert_eq!(first(listed, "permission_mode"), None);
    }

    #[test]
    fn an_array_is_empty_or_not() {
        let done = r#"{"hook_event_name":"Stop","background_tasks":[],"session_crons":[]}"#;
        assert_eq!(array_has_items(done, "background_tasks"), Some(false));
        let busy = r#"{"background_tasks":[ {"id":"t1","type":"shell","status":"running"} ]}"#;
        assert_eq!(array_has_items(busy, "background_tasks"), Some(true));
        assert_eq!(array_has_items(r#"{"a":1}"#, "background_tasks"), None);
        // the key quoted as somebody's value, then the real one
        let decoy = r#"{"fields":["background_tasks"],"background_tasks":[ ]}"#;
        assert_eq!(array_has_items(decoy, "background_tasks"), Some(false));
    }

    #[test]
    fn a_non_string_value_is_not_a_match() {
        assert_eq!(first(r#"{"n":12}"#, "n"), None);
        assert_eq!(field(r#"{"n":12}"#, "n"), "");
    }
}
