//! Just enough ANSI to render a captured pane.
//!
//! The preview shows a live agent session, which is nothing but colour, so
//! `capture-pane -e` is what has to be drawn. fzf gets this for free by being
//! handed the bytes and parsing them itself; a native picker has to do it.
//!
//! Deliberately not a terminal emulator, and deliberately not a dependency.
//! `capture-pane -e` emits SGR (`ESC [ ... m`) and nothing else: the cursor
//! movement, scrolling and mode changes a real stream carries have already been
//! applied to the screen it hands back. So this understands SGR, skips any other
//! escape sequence whole, and never has to be right about anything else.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// One SGR run: the parameters seen so far, applied to a fresh style.
///
/// Rebuilt from scratch on each `m` rather than mutated in place, because the
/// parameters within one sequence are ordered (`0;1;33` is reset, then bold, then
/// yellow) and an accumulated style has to honour a `0` in the middle of it.
fn apply(style: Style, params: &[i32]) -> Style {
    let mut s = style;
    let mut i = 0;
    while i < params.len() {
        let p = params[i];
        match p {
            0 => s = Style::default(),
            1 => s = s.add_modifier(Modifier::BOLD),
            2 => s = s.add_modifier(Modifier::DIM),
            3 => s = s.add_modifier(Modifier::ITALIC),
            4 => s = s.add_modifier(Modifier::UNDERLINED),
            7 => s = s.add_modifier(Modifier::REVERSED),
            9 => s = s.add_modifier(Modifier::CROSSED_OUT),
            21 | 22 => s = s.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => s = s.remove_modifier(Modifier::ITALIC),
            24 => s = s.remove_modifier(Modifier::UNDERLINED),
            27 => s = s.remove_modifier(Modifier::REVERSED),
            29 => s = s.remove_modifier(Modifier::CROSSED_OUT),
            30..=37 => s = s.fg(basic(p - 30)),
            39 => s = s.fg(Color::Reset),
            40..=47 => s = s.bg(basic(p - 40)),
            49 => s = s.bg(Color::Reset),
            90..=97 => s = s.fg(bright(p - 90)),
            100..=107 => s = s.bg(bright(p - 100)),
            // 38/48 take their colour from the parameters that follow: `5;N` for
            // one of the 256, `2;r;g;b` for a true colour. Anything else is a
            // sequence we do not know, and skipping just the introducer would
            // leave its arguments to be read as colours of their own.
            38 | 48 => {
                let fg = p == 38;
                match params.get(i + 1) {
                    Some(5) => {
                        if let Some(&n) = params.get(i + 2) {
                            let c = Color::Indexed(n.clamp(0, 255) as u8);
                            s = if fg { s.fg(c) } else { s.bg(c) };
                        }
                        i += 2;
                    }
                    Some(2) => {
                        if let (Some(&r), Some(&g), Some(&b)) =
                            (params.get(i + 2), params.get(i + 3), params.get(i + 4))
                        {
                            let c = Color::Rgb(r as u8, g as u8, b as u8);
                            s = if fg { s.fg(c) } else { s.bg(c) };
                        }
                        i += 4;
                    }
                    _ => break,
                }
            }
            _ => {}
        }
        i += 1;
    }
    s
}

fn basic(n: i32) -> Color {
    match n {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        _ => Color::Gray,
    }
}

fn bright(n: i32) -> Color {
    match n {
        0 => Color::DarkGray,
        1 => Color::LightRed,
        2 => Color::LightGreen,
        3 => Color::LightYellow,
        4 => Color::LightBlue,
        5 => Color::LightMagenta,
        6 => Color::LightCyan,
        _ => Color::White,
    }
}

/// Text into styled lines, carrying the style across line breaks the way a
/// terminal does: a capture can open a colour on one row and close it on the next.
pub fn to_lines(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut style = Style::default();
    for raw in text.lines() {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut buf = String::new();
        let mut it = raw.char_indices().peekable();
        while let Some((_, c)) = it.next() {
            if c != '\x1b' {
                buf.push(c);
                continue;
            }
            // Everything up to the escape keeps the style in force.
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), style));
            }
            match it.peek().map(|&(_, c)| c) {
                Some('[') => {
                    it.next();
                    // A CSI is parameter bytes (0x30-0x3F, so digits and `;` but
                    // also the private markers `?<=>`), then intermediate bytes
                    // (0x20-0x2F), then one final byte (0x40-0x7E). Stopping at
                    // the first non-digit instead treats the `?` of `ESC[?25l` as
                    // the end of the sequence and prints "25l" on the screen.
                    let mut params: Vec<i32> = Vec::new();
                    let mut num = String::new();
                    let mut private = false;
                    let mut kind = None;
                    for (_, c) in it.by_ref() {
                        match c {
                            '0'..='9' => num.push(c),
                            ';' | ':' => {
                                params.push(num.parse().unwrap_or(0));
                                num.clear();
                            }
                            '?' | '<' | '=' | '>' => private = true,
                            ' '..='/' => {} // intermediate
                            _ => {
                                kind = Some(c);
                                break;
                            }
                        }
                    }
                    if !num.is_empty() || params.is_empty() {
                        params.push(num.parse().unwrap_or(0));
                    }
                    // Only SGR changes anything. Any other final byte is a
                    // sequence this does not speak, and it is dropped rather than
                    // printed, which is the whole job.
                    if kind == Some('m') && !private {
                        style = apply(style, &params);
                    }
                }
                // OSC and the rest: swallow to the terminator so the payload does
                // not land on screen as text.
                Some(']') => {
                    for (_, c) in it.by_ref() {
                        if c == '\x07' || c == '\x1b' {
                            break;
                        }
                    }
                }
                _ => {
                    it.next();
                }
            }
        }
        if !buf.is_empty() {
            spans.push(Span::styled(buf, style));
        }
        lines.push(Line::from(spans));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(l: &Line) -> Vec<String> {
        l.spans.iter().map(|s| s.content.to_string()).collect()
    }

    #[test]
    fn plain_text_is_one_span() {
        let l = to_lines("hello");
        assert_eq!(texts(&l[0]), vec!["hello"]);
        assert_eq!(l[0].spans[0].style, Style::default());
    }

    #[test]
    fn a_colour_opens_a_span_and_a_reset_closes_it() {
        let l = to_lines("a\x1b[31mred\x1b[0mb");
        assert_eq!(texts(&l[0]), vec!["a", "red", "b"]);
        assert_eq!(l[0].spans[1].style.fg, Some(Color::Red));
        assert_eq!(l[0].spans[2].style, Style::default());
    }

    /// The parameters within one sequence are ordered, so a reset in the middle
    /// of one has to clear what came before it.
    #[test]
    fn a_reset_inside_a_sequence_clears_what_preceded_it() {
        let l = to_lines("\x1b[1;33mx\x1b[0;36my");
        assert!(l[0].spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(l[0].spans[1].style.fg, Some(Color::Cyan));
        assert!(!l[0].spans[1].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn indexed_and_true_colour_both_land() {
        let l = to_lines("\x1b[38;5;208mo\x1b[38;2;18;52;86mt");
        assert_eq!(l[0].spans[0].style.fg, Some(Color::Indexed(208)));
        assert_eq!(l[0].spans[1].style.fg, Some(Color::Rgb(18, 52, 86)));
    }

    /// The arguments of a 256-colour sequence must not be read as colours in
    /// their own right: `38;5;1` is orange-ish 1, not red then something.
    #[test]
    fn the_arguments_of_an_extended_colour_are_consumed() {
        let l = to_lines("\x1b[38;5;1;1mx");
        assert_eq!(l[0].spans[0].style.fg, Some(Color::Indexed(1)));
        assert!(l[0].spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    /// Style carries across a line break, the way a terminal does: a capture can
    /// open a colour on one row and close it on the next.
    #[test]
    fn style_carries_from_one_line_to_the_next() {
        let l = to_lines("\x1b[32mgreen\nstill green\x1b[0m\nplain");
        assert_eq!(l[1].spans[0].style.fg, Some(Color::Green));
        assert_eq!(l[2].spans[0].style, Style::default());
    }

    /// Anything that is not SGR is dropped rather than printed. A capture should
    /// not contain cursor movement, but a pane holding a raw log might.
    #[test]
    fn a_non_sgr_sequence_is_swallowed_whole() {
        let l = to_lines("a\x1b[2Jb\x1b[?25lc");
        assert_eq!(texts(&l[0]).concat(), "abc");
    }

    #[test]
    fn an_osc_title_does_not_land_on_screen() {
        let l = to_lines("a\x1b]0;a window title\x07b");
        assert_eq!(texts(&l[0]).concat(), "ab");
    }

    #[test]
    fn a_bare_escape_at_the_end_does_not_panic() {
        assert_eq!(to_lines("a\x1b").len(), 1);
        assert_eq!(to_lines("\x1b[").len(), 1);
        assert_eq!(to_lines("\x1b[38;5").len(), 1);
    }

    /// Blank lines are kept: a captured screen is padded to the pane height, and
    /// dropping the empties would close gaps the session actually has.
    #[test]
    fn blank_lines_survive() {
        assert_eq!(to_lines("a\n\nb").len(), 3);
    }
}
