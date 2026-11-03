//! Removing terminal escape sequences from output text.

/// Remove ANSI escape sequences from `s`, for text shown somewhere other
/// than a terminal: kernels color tracebacks and some print hyperlinks.
///
/// - CSI (`ESC [`): parameter and intermediate bytes, up to a final byte
///   in `0x40..=0x7E`, so `ESC [ 2 ~` goes as well as SGR colors.
/// - OSC (`ESC ]`), such as OSC 8 hyperlinks: up to BEL or ST (`ESC \`).
///   The link's text, between two OSC 8 sequences, stays.
/// - Any other `ESC x` pair. A lone `ESC` at the end is dropped.
///
/// An unterminated sequence runs to the end of the text.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::strip_ansi;

    #[test]
    fn sgr_colors() {
        assert_eq!(strip_ansi("\u{1b}[31mErr\u{1b}[0m: x"), "Err: x");
        assert_eq!(strip_ansi("\u{1b}[1;38;5;196mbold\u{1b}[m"), "bold");
    }

    #[test]
    fn csi_ending_in_tilde_keeps_the_text_after_it() {
        assert_eq!(strip_ansi("a\u{1b}[2~bc"), "abc");
        assert_eq!(strip_ansi("\u{1b}[200~pasted\u{1b}[201~"), "pasted");
    }

    #[test]
    fn osc_8_links_keep_their_text() {
        assert_eq!(
            strip_ansi("see \u{1b}]8;;http://a\u{7}link\u{1b}]8;;\u{7} now"),
            "see link now"
        );
        // Ended by ST instead of BEL.
        assert_eq!(
            strip_ansi("\u{1b}]8;;http://a\u{1b}\\link\u{1b}]8;;\u{1b}\\"),
            "link"
        );
    }

    #[test]
    fn plain_text_and_odd_endings() {
        assert_eq!(strip_ansi("no escapes: é ✓"), "no escapes: é ✓");
        assert_eq!(strip_ansi("x\u{1b}"), "x");
        assert_eq!(strip_ansi("x\u{1b}[31"), "x");
        assert_eq!(strip_ansi("x\u{1b}]8;;http://a"), "x");
        assert_eq!(strip_ansi("a\u{1b}cb"), "ab");
    }
}
