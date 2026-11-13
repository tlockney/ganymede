//! A small syntax highlighter for the published page: keywords, strings,
//! comments and numbers as `<span>`s, enough to read code at a glance.
//! It runs on the server, so the page needs no script, and it knows a few
//! languages by hand rather than pulling in a grammar library. Anything it
//! doesn't know, or can't follow (a JavaScript regex literal, say), comes
//! out as plain text: it never drops or reorders a character.

use crate::html::esc;

/// What a language's code looks like to the highlighter.
struct Lang {
    keywords: &'static [&'static str],
    /// `true`, `None` and the like, shown as numbers are.
    literals: &'static [&'static str],
    line_comment: Option<&'static str>,
    block_comment: Option<(&'static str, &'static str)>,
    /// Quotes that open a string ending on the same line.
    quotes: &'static [char],
    /// Backtick template strings, which can span lines.
    backtick: bool,
    /// Python: `"""` / `'''` strings and `r"..."` / `f"..."` prefixes.
    python_strings: bool,
    /// Shell: `#` starts a comment only at the start of a word.
    shell: bool,
    /// Characters an identifier may hold besides letters, digits and `_`.
    ident_extra: &'static [char],
}

#[rustfmt::skip]
const TS: Lang = Lang {
    keywords: &[
        "abstract", "as", "async", "await", "break", "case", "catch", "class", "const",
        "continue", "debugger", "declare", "default", "delete", "do", "else", "enum", "export",
        "extends", "finally", "for", "from", "function", "if", "implements", "import",
        "in", "instanceof", "interface", "keyof", "let", "namespace", "new", "of", "private",
        "protected", "public", "readonly", "return", "satisfies", "static", "super",
        "switch", "this", "throw", "try", "type", "typeof", "var", "void", "while", "with",
        "yield",
    ],
    literals: &["true", "false", "null", "undefined", "NaN", "Infinity"],
    line_comment: Some("//"),
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    backtick: true,
    python_strings: false,
    shell: false,
    ident_extra: &['$'],
};

#[rustfmt::skip]
const PYTHON: Lang = Lang {
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "case", "class", "continue", "def",
        "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
        "in", "is", "lambda", "match", "nonlocal", "not", "or", "pass", "raise", "return",
        "try", "while", "with", "yield",
    ],
    literals: &["True", "False", "None"],
    line_comment: Some("#"),
    block_comment: None,
    quotes: &['"', '\''],
    backtick: false,
    python_strings: true,
    shell: false,
    ident_extra: &[],
};

#[rustfmt::skip]
const SHELL: Lang = Lang {
    keywords: &[
        "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if",
        "in", "local", "return", "then", "until", "while",
    ],
    literals: &[],
    line_comment: Some("#"),
    block_comment: None,
    quotes: &['"', '\''],
    backtick: false,
    python_strings: false,
    shell: true,
    ident_extra: &['-'],
};

const JSON: Lang = Lang {
    keywords: &[],
    literals: &["true", "false", "null"],
    line_comment: None,
    block_comment: None,
    quotes: &['"'],
    backtick: false,
    python_strings: false,
    shell: false,
    ident_extra: &[],
};

/// The highlighter for a language name, as a kernel's `language_info` or
/// a Markdown fence names it.
fn lang(name: &str) -> Option<&'static Lang> {
    let name = name.trim().to_ascii_lowercase();
    // A fence's info string can carry more after the language.
    let name = name
        .split(|c: char| c.is_whitespace() || c == ',' || c == '{')
        .next()
        .unwrap_or("");
    match name {
        "typescript" | "ts" | "tsx" | "javascript" | "js" | "jsx" | "mjs" | "cjs" | "node"
        | "deno" => Some(&TS),
        "python" | "python3" | "py" | "ipython" | "ipython3" => Some(&PYTHON),
        "bash" | "sh" | "shell" | "zsh" | "console" | "shell-session" => Some(&SHELL),
        "json" | "jsonc" => Some(&JSON),
        _ => None,
    }
}

/// `code` as HTML: escaped, with spans for the tokens of `language` when
/// the highlighter knows it.
pub(crate) fn highlight(code: &str, language: &str) -> String {
    match lang(language) {
        Some(l) => Lexer::new(code, l).run(),
        None => esc(code),
    }
}

struct Lexer<'a> {
    src: &'a str,
    lang: &'static Lang,
    at: usize,
    out: String,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str, lang: &'static Lang) -> Self {
        Lexer {
            src,
            lang,
            at: 0,
            out: String::with_capacity(src.len() * 2),
        }
    }

    fn rest(&self) -> &'a str {
        &self.src[self.at..]
    }

    fn emit(&mut self, class: Option<&str>, end: usize) {
        // Always move on by at least one character, so no rule can stall
        // the loop.
        let end = if end > self.at {
            end
        } else {
            self.at + self.rest().chars().next().map_or(0, char::len_utf8)
        };
        let text = &self.src[self.at..end];
        match class {
            Some(c) => {
                self.out.push_str("<span class=\"");
                self.out.push_str(c);
                self.out.push_str("\">");
                self.out.push_str(&esc(text));
                self.out.push_str("</span>");
            }
            None => self.out.push_str(&esc(text)),
        }
        self.at = end;
    }

    fn run(mut self) -> String {
        while let Some(c) = self.rest().chars().next() {
            let rest = self.rest();
            if let Some(end) = self.comment(rest) {
                self.emit(Some("hl-c"), self.at + end);
            } else if let Some(end) = self.string(rest) {
                self.emit(Some("hl-s"), self.at + end);
            } else if c.is_ascii_digit() || (c == '.' && next_is_digit(rest)) {
                let end = number_end(rest);
                self.emit(Some("hl-n"), self.at + end);
            } else if is_ident_start(c) {
                let end = rest
                    .char_indices()
                    .skip(1)
                    .find(|&(_, c)| !self.ident_char(c))
                    .map_or(rest.len(), |(i, _)| i);
                let word = &rest[..end];
                // A property (`row.type`, `d.default`) is a name, whatever it's called.
                // In shell, so is a path part (`/opt/for/bin`).
                let before = &self.src[..self.at];
                // `...xs` is a spread, not a property.
                let property = (before.ends_with('.') && !before.ends_with("..."))
                    || (self.lang.shell && before.ends_with('/'));
                let class = if property {
                    None
                } else if self.lang.keywords.contains(&word) {
                    Some("hl-k")
                } else if self.lang.literals.contains(&word) {
                    Some("hl-n")
                } else {
                    None
                };
                self.emit(class, self.at + end);
            } else {
                // Plain text up to the next character that may start a token.
                let end = rest
                    .char_indices()
                    .skip(1)
                    .find(|&(_, c)| c.is_alphanumeric() || "_$\"'`#/.".contains(c))
                    .map_or(rest.len(), |(i, _)| i);
                self.emit(None, self.at + end);
            }
        }
        self.out
    }

    fn ident_char(&self, c: char) -> bool {
        c.is_alphanumeric() || c == '_' || self.lang.ident_extra.contains(&c)
    }

    /// The length of a comment starting `rest`.
    fn comment(&self, rest: &str) -> Option<usize> {
        if let Some(open) = self.lang.line_comment {
            if rest.starts_with(open) {
                if self.lang.shell {
                    // `a#b` and `${#x}` aren't comments.
                    let before = self.src[..self.at].chars().next_back();
                    if before.is_some_and(|b| !b.is_whitespace() && !";|&(".contains(b)) {
                        return None;
                    }
                }
                return Some(rest.find('\n').unwrap_or(rest.len()));
            }
        }
        if let Some((open, close)) = self.lang.block_comment {
            if let Some(body) = rest.strip_prefix(open) {
                // Unclosed (a regex such as `/\/*$/`): to the line's end only.
                return Some(body.find(close).map_or_else(
                    || rest.find('\n').unwrap_or(rest.len()),
                    |i| open.len() + i + close.len(),
                ));
            }
        }
        None
    }

    /// The length of a string starting `rest`, prefix included.
    fn string(&self, rest: &str) -> Option<usize> {
        let mut prefix = 0;
        if self.lang.python_strings {
            // `r"`, `f'`, `rb"` and the like: a prefix only when the
            // letters stand alone, not the end of a longer name.
            let letters = rest
                .char_indices()
                .take_while(|(i, c)| *i < 2 && "rRbBfFuU".contains(*c))
                .count();
            let before = self.src[..self.at].chars().next_back();
            if letters > 0
                && rest[letters..].starts_with(['"', '\''])
                && !before.is_some_and(|b| self.ident_char(b))
            {
                prefix = letters;
            }
            let body = &rest[prefix..];
            for triple in ["\"\"\"", "'''"] {
                if body.starts_with(triple) {
                    let end = closing(&body[3..], triple, true).map_or(body.len(), |i| 3 + i + 3);
                    return Some(prefix + end);
                }
            }
        }
        let body = &rest[prefix..];
        let quote = body.chars().next()?;
        // Backtick templates span lines; so do shell strings.
        let multiline = if quote == '`' && self.lang.backtick {
            true
        } else if self.lang.quotes.contains(&quote) {
            self.lang.shell
        } else {
            return None;
        };
        let q = &body[..1];
        // Shell single quotes have no escapes.
        let escapes = !(self.lang.shell && quote == '\'');
        let end = match closing(&body[1..], q, escapes) {
            // A one-line string may continue past a `\` at a line's end.
            Some(i) if multiline || !has_bare_newline(&body[1..1 + i]) => 1 + i + 1,
            // An unclosed string ends at the line's end, so a stray quote
            // (a regex such as `/`/`) can't colour the rest of the cell.
            _ => body.find('\n').unwrap_or(body.len()),
        };
        Some(prefix + end)
    }
}

/// Whether `s` has a line break not escaped by a `\` before it.
fn has_bare_newline(s: &str) -> bool {
    s.match_indices('\n')
        .any(|(i, _)| !s[..i].trim_end_matches('\r').ends_with('\\'))
}

/// Where `close` first stands in `s`, skipping backslash escapes.
fn closing(s: &str, close: &str, escapes: bool) -> Option<usize> {
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        if escapes && c == '\\' {
            chars.next();
        } else if s[i..].starts_with(close) {
            return Some(i);
        }
    }
    None
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '$'
}

fn next_is_digit(rest: &str) -> bool {
    rest.chars().nth(1).is_some_and(|c| c.is_ascii_digit())
}

/// The length of a number: digits, `_`, letters for `0x`, `1e5`, `10n`,
/// `2j`, one `.`, and a sign after an exponent.
fn number_end(rest: &str) -> usize {
    let bytes = rest.as_bytes();
    let hex = rest.starts_with("0x") || rest.starts_with("0X");
    let mut dot = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_alphanumeric() || b == b'_' {
            i += 1;
            if !hex && (b == b'e' || b == b'E') && matches!(bytes.get(i), Some(b'+' | b'-')) {
                i += 1;
            }
        } else if b == b'.' && !dot && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
            dot = true;
            i += 1;
        } else {
            break;
        }
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The highlighted code with its tags taken out and entities undone.
    fn text(html: &str) -> String {
        let mut out = String::new();
        let mut tag = false;
        for c in html.chars() {
            match c {
                '<' => tag = true,
                '>' if tag => tag = false,
                c if !tag => out.push(c),
                _ => {}
            }
        }
        out.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&")
    }

    #[test]
    fn typescript() {
        let code = "// load\nconst n = await rows(\"a<b\", `x ${y}\nz`) /* two */ + 1.5e-3;\nif (n === null) return 0x1F;";
        let html = highlight(code, "typescript");
        assert_eq!(text(&html), code);
        assert!(html.contains("<span class=\"hl-c\">// load</span>"));
        assert!(html.contains("<span class=\"hl-k\">const</span>"));
        assert!(html.contains("<span class=\"hl-k\">await</span>"));
        assert!(html.contains("<span class=\"hl-s\">&quot;a&lt;b&quot;</span>"));
        assert!(html.contains("<span class=\"hl-s\">`x ${y}\nz`</span>"));
        assert!(html.contains("<span class=\"hl-c\">/* two */</span>"));
        assert!(html.contains("<span class=\"hl-n\">1.5e-3</span>"));
        assert!(html.contains("<span class=\"hl-n\">null</span>"));
        assert!(html.contains("<span class=\"hl-n\">0x1F</span>"));
        assert!(!html.contains("<span class=\"hl-k\">rows</span>"));
        assert!(!highlight("row.type", "ts").contains("hl-k"));
    }

    #[test]
    fn python() {
        let code = "def f(x):\n    \"\"\"Doc \"quoted\".\"\"\"\n    return rf'{x}\\'' if x is None else 2 # done\nprint(\"a\")";
        let html = highlight(code, "python");
        assert_eq!(text(&html), code);
        assert!(html.contains("<span class=\"hl-k\">def</span>"));
        assert!(html.contains("<span class=\"hl-s\">&quot;&quot;&quot;Doc &quot;quoted&quot;.&quot;&quot;&quot;</span>"));
        assert!(html.contains("<span class=\"hl-s\">rf&#39;{x}\\&#39;&#39;</span>"));
        assert!(html.contains("<span class=\"hl-n\">None</span>"));
        assert!(html.contains("<span class=\"hl-c\"># done</span>"));
        // `print` isn't a keyword, and `if` inside a name isn't one.
        assert!(!html.contains("hl-k\">print"));
        let name = highlight("elif_count = 1", "python");
        assert!(!name.contains("hl-k"));
    }

    #[test]
    fn shell() {
        let code = "curl -s 'https://x/a#b' -o data.csv # journal\necho ${#arr} \"$HOME\"";
        let html = highlight(code, "bash");
        assert_eq!(text(&html), code);
        assert!(html.contains("<span class=\"hl-s\">&#39;https://x/a#b&#39;</span>"));
        assert!(html.contains("<span class=\"hl-c\"># journal</span>"));
        assert!(!html.contains("hl-c\">#arr"));
    }

    #[test]
    fn unclosed_strings_end_at_the_line() {
        // A regex literal with a quote must not swallow the next lines.
        let block = highlight("s.replace(/\\/*$/, \"\");\nconst n = 1;", "ts");
        assert!(
            block.contains("<span class=\"hl-k\">const</span> n"),
            "{block}"
        );
        assert!(highlight("[...new Set(xs)]", "ts").contains("<span class=\"hl-k\">new</span>"));
        let jq = highlight("jq '\n.a | if . then 1 else 2 end\n' f", "sh");
        assert!(jq.contains("<span class=\"hl-s\">&#39;\n.a"), "{jq}");
        let cont = highlight("s = 'a\\\nb' if x else 2", "python");
        assert!(
            cont.contains("<span class=\"hl-s\">&#39;a\\\nb&#39;</span> <span class=\"hl-k\">if"),
            "{cont}"
        );
        let tick = highlight("const re = /`/;\nconst n = 1;", "ts");
        assert!(
            tick.contains("<span class=\"hl-k\">const</span> n"),
            "{tick}"
        );
        assert!(!highlight("ls /opt/for/bin", "sh").contains("hl-k"));
        let code = "const r = /'/;\nconst n = 1;";
        let html = highlight(code, "js");
        assert_eq!(text(&html), code);
        assert!(html.contains("<span class=\"hl-k\">const</span> n"));
    }

    #[test]
    fn unknown_language_is_plain() {
        assert_eq!(highlight("fn <main>()", "rust"), "fn &lt;main&gt;()");
        assert_eq!(highlight("x = 1", ""), "x = 1");
        assert!(highlight("x = 1", "python {.class}").contains("hl-n"));
    }

    #[test]
    fn every_character_survives() {
        let samples = [
            "é = 'ü' # ñ\n\"\"\"\n",
            "`unclosed ${",
            "/* open",
            "r'",
            "0x",
            ".5 + 1.",
            "a.b.c(1..2)",
            "\\",
            "$x + @y",
        ];
        for lang in ["ts", "python", "sh", "json"] {
            for s in samples {
                assert_eq!(text(&highlight(s, lang)), s, "{lang}: {s:?}");
            }
        }
    }
}
