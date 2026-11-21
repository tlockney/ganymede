//! A small syntax highlighter for the published page: keywords, strings,
//! comments and numbers as `<span>`s, enough to read code at a glance.
//! It runs on the server, so the page needs no script, and it knows a few
//! languages by hand rather than pulling in a grammar library. Anything it
//! doesn't know, or can't follow (a JavaScript regex literal, say), comes
//! out as plain text: it never drops or reorders a character.
//!
//! In TypeScript, the template literal of a `jupyter.sql` tag or of a
//! `jupyter.sql.query(...)` call is DuckDB SQL, and is highlighted as
//! SQL, with each `${...}` in it as TypeScript, the way an editor's
//! language injection treats a `sql` tag.

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
    /// The byte range of the last comment emitted, so a `jupyter.sql` inside
    /// one doesn't tag the template after it.
    comment: (usize, usize),
    /// Set when a SQL template's end couldn't be found: the rest of the
    /// cell takes the plain-string path, so a hostile cell can't make every
    /// tagged backtick rescan it to the end.
    no_sql: bool,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str, lang: &'static Lang) -> Self {
        Lexer {
            src,
            lang,
            at: 0,
            out: String::with_capacity(src.len() * 2),
            comment: (0, 0),
            no_sql: false,
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
        push_span(&mut self.out, class, &self.src[self.at..end]);
        self.at = end;
    }

    fn run(mut self) -> String {
        while let Some(c) = self.rest().chars().next() {
            let rest = self.rest();
            // A template whose end can't be found (a quote or regex in a
            // `${...}` the scan misreads, or nesting too deep) is a plain
            // string, as it was before SQL: up to its backtick, or the line.
            let mut sql_close = None;
            if c == '`' && !self.no_sql && self.sql_tagged() {
                sql_close = template_end(&rest[1..], 0);
                self.no_sql = sql_close.is_none();
            }
            if let Some(close) = sql_close {
                push_span(&mut self.out, Some("hl-s"), "`");
                sql_template_body(&mut self.out, &rest[1..1 + close]);
                push_span(&mut self.out, Some("hl-s"), "`");
                self.at += close + 2;
            } else if let Some(end) = self.comment(rest) {
                self.comment = (self.at, self.at + end);
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

    /// Whether a backtick here opens a SQL template: `` jupyter.sql`...` ``,
    /// or the first argument of `` jupyter.sql.query(`...`) ``.
    fn sql_tagged(&self) -> bool {
        if !std::ptr::eq(self.lang, &TS) {
            return false;
        }
        let before = self.src[..self.at].trim_end();
        let before = match before.strip_suffix('(') {
            Some(call) => match call.trim_end().strip_suffix(".query") {
                Some(tag) => tag,
                None => return false,
            },
            None => before,
        };
        // Not a tag named in a comment (`// see jupyter.sql`).
        let (from, to) = self.comment;
        if from < before.len() && before.len() <= to {
            return false;
        }
        before
            .strip_suffix("jupyter.sql")
            .is_some_and(|head| !head.ends_with(|c: char| self.ident_char(c) || c == '.'))
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

#[rustfmt::skip]
const SQL_KEYWORDS: &[&str] = &[
    "add", "all", "alter", "and", "anti", "any", "array", "as", "asc", "asof", "attach", "between",
    "bigint", "blob", "bool", "boolean", "by", "case", "cast", "column", "copy", "create",
    "cross", "cube", "current", "date", "decimal", "default", "delete", "desc", "describe",
    "detach", "distinct", "double", "drop", "else", "end", "except", "exclude", "exists",
    "explain", "filter", "first", "float", "following", "for", "from", "full", "group",
    "having", "hugeint", "if", "ilike", "in", "inner", "insert", "install", "int", "integer",
    "intersect", "interval", "into", "is", "join", "last", "lateral", "left", "like", "limit",
    "load", "macro", "natural", "not", "nulls", "numeric", "offset", "on", "or", "order",
    "outer", "over", "partition", "pivot", "positional", "pragma", "preceding", "primary",
    "qualify", "range", "real", "recursive", "replace", "returning", "right", "rollup", "rows",
    "sample", "select", "semi", "set", "show", "smallint", "summarize", "table", "temp",
    "temporary", "then", "time", "timestamp", "timestamptz", "to", "try_cast", "unbounded",
    "union", "unpivot", "update", "using", "values", "varchar", "view", "when", "where",
    "window", "with", "within",
];

/// The SQL lexer's state between the pieces of a template that its
/// `${...}`s cut apart: `'${FROM}'` is one string in two pieces.
#[derive(Clone, Copy)]
enum Sql {
    Code,
    /// In a `'` string or a `"` quoted name.
    Quoted(u8),
    LineComment,
    BlockComment,
}

/// The body of a SQL template (between its backticks) as HTML: SQL, with
/// each `${...}` highlighted as TypeScript.
fn sql_template_body(out: &mut String, body: &str) {
    let mut state = Sql::Code;
    let mut rest = body;
    loop {
        let cut = next_interpolation(rest).unwrap_or(rest.len());
        state = sql_text(out, &rest[..cut], state);
        if cut == rest.len() {
            return;
        }
        let expr = &rest[cut + 2..];
        out.push_str("${");
        match interpolation_end(expr, 0) {
            Some(end) => {
                out.push_str(&Lexer::new(&expr[..end], &TS).run());
                out.push('}');
                rest = &expr[end + 1..];
            }
            None => {
                out.push_str(&Lexer::new(expr, &TS).run());
                return;
            }
        }
    }
}

/// One piece of SQL between interpolations, starting in `state`; returns
/// the state at its end.
fn sql_text(out: &mut String, text: &str, mut state: Sql) -> Sql {
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        let (class, len, next) = match state {
            Sql::Quoted(q) => match sql_closing(rest, q) {
                Some(i) => (quoted_class(q), i, Sql::Code),
                None => (quoted_class(q), rest.len(), state),
            },
            Sql::LineComment => match rest.find('\n') {
                Some(i) => (Some("hl-c"), i, Sql::Code),
                None => (Some("hl-c"), rest.len(), state),
            },
            Sql::BlockComment => match rest.find("*/") {
                Some(i) => (Some("hl-c"), i + 2, Sql::Code),
                None => (Some("hl-c"), rest.len(), state),
            },
            Sql::Code => sql_token(rest, &text[..at]),
        };
        // A token that ends at the piece's end leaves its state open, but
        // an empty run never happens: every token holds a character.
        let end = at + len.max(rest.chars().next().map_or(0, char::len_utf8));
        push_span(out, class, &text[at..end]);
        at = end;
        state = next;
    }
    state
}

/// The SQL token at the start of `rest`, after `before`: its class, its
/// length, and the state after it.
fn sql_token(rest: &str, before: &str) -> (Option<&'static str>, usize, Sql) {
    let c = rest.chars().next().unwrap_or(' ');
    if rest.starts_with("--") {
        match rest.find('\n') {
            Some(i) => (Some("hl-c"), i, Sql::Code),
            None => (Some("hl-c"), rest.len(), Sql::LineComment),
        }
    } else if let Some(body) = rest.strip_prefix("/*") {
        match body.find("*/") {
            Some(i) => (Some("hl-c"), 2 + i + 2, Sql::Code),
            None => (Some("hl-c"), rest.len(), Sql::BlockComment),
        }
    } else if let (q @ (b'\'' | b'"'), width) = sql_char(rest.as_bytes(), 0) {
        match sql_closing(&rest[width..], q) {
            Some(i) => (quoted_class(q), width + i, Sql::Code),
            None => (quoted_class(q), rest.len(), Sql::Quoted(q)),
        }
    } else if c == '\\' && rest.len() > 1 && sql_char(rest.as_bytes(), 0).1 == 2 {
        // A template escape that is no quote: `\\`, `` \` `` and `\$`.
        (None, 2, Sql::Code)
    } else if c.is_ascii_digit() || (c == '.' && next_is_digit(rest)) {
        (Some("hl-n"), number_end(rest), Sql::Code)
    } else if c.is_alphabetic() || c == '_' {
        let end = rest
            .char_indices()
            .find(|&(_, c)| !(c.is_alphanumeric() || c == '_' || c == '$'))
            .map_or(rest.len(), |(i, _)| i);
        let word = rest[..end].to_ascii_lowercase();
        // A qualified name (`t.date`) is a name, whatever it's called.
        let class = if before.ends_with('.') {
            None
        } else if SQL_KEYWORDS.contains(&word.as_str()) {
            Some("hl-k")
        } else if ["true", "false", "null"].contains(&word.as_str()) {
            Some("hl-n")
        } else {
            None
        };
        (class, end, Sql::Code)
    } else {
        let end = rest
            .char_indices()
            .skip(1)
            .find(|&(_, c)| c.is_alphanumeric() || "_'\"-/\\.".contains(c))
            .map_or(rest.len(), |(i, _)| i);
        (None, end, Sql::Code)
    }
}

/// A SQL string is a string; a `"quoted name"` is a name.
fn quoted_class(q: u8) -> Option<&'static str> {
    (q == b'\'').then_some("hl-s")
}

/// Where the quote `q` closes in `s`, as the offset after it: a doubled
/// quote is SQL's escape.
fn sql_closing(s: &str, q: u8) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let (c, width) = sql_char(b, i);
        i += width;
        if c == q {
            match sql_char(b, i) {
                (c, width) if c == q && i < b.len() => i += width,
                _ => return Some(i),
            }
        }
    }
    None
}

/// The SQL character at `i` in template text and its width. The kernel
/// joins the template's cooked strings, so `\'` and `\"` reach DuckDB as
/// the quote itself (DuckDB strings have no backslash escape), while
/// `\\`, `` \` `` and `\$` stand for a character that is none.
fn sql_char(b: &[u8], i: usize) -> (u8, usize) {
    match (b.get(i), b.get(i + 1)) {
        (Some(b'\\'), Some(&n @ (b'\'' | b'"'))) => (n, 2),
        (Some(b'\\'), Some(b'\\' | b'`' | b'$')) => (b'\\', 2),
        (Some(&c), _) => (c, 1),
        (None, _) => (0, 1),
    }
}

/// Where the first `${` not escaped by a `\` stands in template text.
fn next_interpolation(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'$' if b.get(i + 1) == Some(&b'{') => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// How deep templates and interpolations may nest before the scan gives
/// up, so the two functions below can't overflow the stack.
const MAX_NESTING: usize = 64;

/// Where the closing backtick of a template stands, `s` starting just
/// after the opening one; `` ` ``s inside its `${...}`s don't count.
/// `None` when it never closes, or nests past `MAX_NESTING`.
fn template_end(s: &str, depth: usize) -> Option<usize> {
    if depth > MAX_NESTING {
        return None;
    }
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'`' => return Some(i),
            b'$' if b.get(i + 1) == Some(&b'{') => {
                i += 2 + interpolation_end(&s[i + 2..], depth + 1)? + 1
            }
            _ => i += 1,
        }
    }
    None
}

/// Where the `}` closing an interpolation stands, `s` starting just
/// after its `${`: braces nest, and strings, templates and comments in
/// it are skipped.
fn interpolation_end(s: &str, depth: usize) -> Option<usize> {
    if depth > MAX_NESTING {
        return None;
    }
    let b = s.as_bytes();
    let mut braces = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'{' => braces += 1,
            b'}' if braces == 0 => return Some(i),
            b'}' => braces -= 1,
            q @ (b'\'' | b'"') => {
                let quote = if q == b'"' { "\"" } else { "'" };
                i += 1 + closing(&s[i + 1..], quote, true)?;
            }
            b'`' => i += 1 + template_end(&s[i + 1..], depth + 1)?,
            b'/' if b.get(i + 1) == Some(&b'/') => i += s[i..].find('\n')?,
            b'/' if b.get(i + 1) == Some(&b'*') => i += 2 + s[i + 2..].find("*/")? + 1,
            _ => {}
        }
        i += 1;
    }
    None
}

fn push_span(out: &mut String, class: Option<&str>, text: &str) {
    match class {
        Some(c) => {
            out.push_str("<span class=\"");
            out.push_str(c);
            out.push_str("\">");
            out.push_str(&esc(text));
            out.push_str("</span>");
        }
        None => out.push_str(&esc(text)),
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

    /// The `<span>`s of `html` in order, each as (class, text).
    fn spans(html: &str) -> Vec<(String, String)> {
        html.split("<span class=\"")
            .skip(1)
            .map(|s| {
                let (class, rest) = s.split_once("\">").unwrap();
                let body = rest.split_once("</span>").unwrap().0;
                (class.to_owned(), text(body))
            })
            .collect()
    }

    fn span(class: &str, text: &str) -> (String, String) {
        (class.to_owned(), text.to_owned())
    }

    #[test]
    fn sql_tag() {
        let code = "await jupyter.sql`\n  select class, count(*) as points\n  from meter_points where class <> 'TOTAL' group by class`;";
        let html = highlight(code, "typescript");
        assert_eq!(text(&html), code);
        assert_eq!(
            spans(&html),
            [
                span("hl-k", "await"),
                span("hl-s", "`"),
                span("hl-k", "select"),
                span("hl-k", "as"),
                span("hl-k", "from"),
                span("hl-k", "where"),
                span("hl-s", "'TOTAL'"),
                span("hl-k", "group"),
                span("hl-k", "by"),
                span("hl-s", "`"),
            ]
        );
        // `class` and `count` are names in SQL, though `class` is a
        // TypeScript keyword.
        assert!(
            html.contains("<span class=\"hl-k\">select</span> class, count(*)"),
            "{html}"
        );
    }

    #[test]
    fn sql_query_call() {
        let code = "await jupyter.sql.query(`\n  create or replace table meter_rows as\n  select start, rate::double as rate -- per hour\n  from read_parquet('s3://${BUCKET}/x/s=${SENSOR}/*.parquet')\n  where d between date '${FROM}' - 2 and date '${TO}' + 1`);";
        let html = highlight(code, "typescript");
        assert_eq!(text(&html), code);
        assert_eq!(
            spans(&html),
            [
                span("hl-k", "await"),
                span("hl-s", "`"),
                span("hl-k", "create"),
                span("hl-k", "or"),
                span("hl-k", "replace"),
                span("hl-k", "table"),
                span("hl-k", "as"),
                span("hl-k", "select"),
                span("hl-k", "double"),
                span("hl-k", "as"),
                span("hl-c", "-- per hour"),
                span("hl-k", "from"),
                span("hl-s", "'s3://"),
                span("hl-s", "/x/s="),
                span("hl-s", "/*.parquet'"),
                span("hl-k", "where"),
                span("hl-k", "between"),
                span("hl-k", "date"),
                span("hl-s", "'"),
                span("hl-s", "'"),
                span("hl-n", "2"),
                span("hl-k", "and"),
                span("hl-k", "date"),
                span("hl-s", "'"),
                span("hl-s", "'"),
                span("hl-n", "1"),
                span("hl-s", "`"),
            ]
        );
        // The interpolations sit between the string's pieces, outside
        // any span, and the string closes after them.
        assert!(html.contains("<span class=\"hl-s\">&#39;s3://</span>${BUCKET}<span class=\"hl-s\">/x/s=</span>${SENSOR}<span"), "{html}");
        assert!(html.contains("<span class=\"hl-k\">date</span> <span class=\"hl-s\">&#39;</span>${FROM}<span class=\"hl-s\">&#39;</span> - <span class=\"hl-n\">2</span>"), "{html}");
        // The comment ends at the line, and the SQL goes on.
        assert!(
            html.contains(
                "<span class=\"hl-c\">-- per hour</span>\n  <span class=\"hl-k\">from</span>"
            ),
            "{html}"
        );
    }

    #[test]
    fn sql_interpolation_is_typescript() {
        let code = "jupyter.sql`select * from t where n > ${await count(\"}\", `a${b}`)} and s = 'it''s' limit ${new Date().getDay()}`; const x = 1;";
        let html = highlight(code, "ts");
        assert_eq!(text(&html), code);
        assert!(html.contains("${<span class=\"hl-k\">await</span> count(<span class=\"hl-s\">&quot;}&quot;</span>, <span class=\"hl-s\">`a${b}`</span>)}"), "{html}");
        assert!(
            html.contains("<span class=\"hl-s\">&#39;it&#39;&#39;s&#39;</span>"),
            "{html}"
        );
        assert!(
            html.contains("${<span class=\"hl-k\">new</span> Date().getDay()}"),
            "{html}"
        );
        // After the template, TypeScript again.
        assert!(
            html.contains(
                "`</span>; <span class=\"hl-k\">const</span> x = <span class=\"hl-n\">1</span>;"
            ),
            "{html}"
        );
    }

    #[test]
    fn other_templates_stay_strings() {
        let code = "const q = `select * from t where d = '${FROM}'`;\nsql`select 1`; db.sql`select 1`; myjupyter.sql`select 1`; jupyter.sql.run(`select 1`); jupyter.sql.query(x, `select 1`);";
        let html = highlight(code, "ts");
        assert_eq!(text(&html), code);
        assert!(
            html.contains(
                "<span class=\"hl-s\">`select * from t where d = &#39;${FROM}&#39;`</span>"
            ),
            "{html}"
        );
        assert_eq!(
            html.matches("<span class=\"hl-s\">`select 1`</span>")
                .count(),
            5,
            "{html}"
        );
        assert!(!html.contains("hl-k\">select"), "{html}");
        // In another language, no SQL either.
        assert!(!highlight("jupyter.sql`select 1`", "python").contains("hl-k"));
    }

    #[test]
    fn sql_templates_keep_every_character() {
        let samples = [
            "jupyter.sql`",
            "jupyter.sql`select '",
            "jupyter.sql`select ${",
            "jupyter.sql`select ${x",
            "jupyter.sql`-- a ${x} b\nselect /* c ${y} */ 1`",
            "jupyter.sql`select \"from\" from t where a = 'é''${x}\\`'`",
            "jupyter.sql.query(  `select 1`)",
            "jupyter.sql\n`select 1` + `x`",
            "jupyter.sql`\\${x} ${'`'} ${`${`}`}`}`",
        ];
        for s in samples {
            let html = highlight(s, "ts");
            assert_eq!(text(&html), s, "{s:?}: {html}");
        }
        // A quoted name is no keyword, and an escaped `${` no interpolation.
        let html = highlight("jupyter.sql`select \"from\", '\\${x}'`", "ts");
        assert!(!html.contains("hl-k\">from"), "{html}");
        assert!(
            html.contains("<span class=\"hl-s\">&#39;\\${x}&#39;</span>"),
            "{html}"
        );
    }

    #[test]
    fn sql_names_and_literals() {
        // A qualified name (`t.date`) is a name, though `date` is a keyword.
        let html = highlight("jupyter.sql`select t.date, t.from from t`", "ts");
        assert_eq!(
            spans(&html),
            [
                span("hl-s", "`"),
                span("hl-k", "select"),
                span("hl-k", "from"),
                span("hl-s", "`"),
            ]
        );
        let html = highlight("jupyter.sql`select true, FALSE, Null`", "ts");
        assert_eq!(
            spans(&html),
            [
                span("hl-s", "`"),
                span("hl-k", "select"),
                span("hl-n", "true"),
                span("hl-n", "FALSE"),
                span("hl-n", "Null"),
                span("hl-s", "`"),
            ]
        );
    }

    #[test]
    fn sql_template_end_skips_what_is_in_an_interpolation() {
        // A `//` comment holds a `}` and a backtick; neither ends anything.
        let code = "jupyter.sql`select ${a // }`\n} from t`; const x = 1;";
        let html = highlight(code, "ts");
        assert_eq!(text(&html), code);
        assert_eq!(
            spans(&html),
            [
                span("hl-s", "`"),
                span("hl-k", "select"),
                span("hl-c", "// }`"),
                span("hl-k", "from"),
                span("hl-s", "`"),
                span("hl-k", "const"),
                span("hl-n", "1"),
            ]
        );
        // An object literal's braces nest, so its `}` isn't the end.
        let code = "jupyter.sql`select ${ {k:1}.k + `a` } from t`; const x = 1;";
        let html = highlight(code, "ts");
        assert_eq!(text(&html), code);
        assert_eq!(
            spans(&html),
            [
                span("hl-s", "`"),
                span("hl-k", "select"),
                span("hl-n", "1"),
                span("hl-s", "`a`"),
                span("hl-k", "from"),
                span("hl-s", "`"),
                span("hl-k", "const"),
                span("hl-n", "1"),
            ]
        );
        // A template inside the interpolation is skipped whole, so its
        // `}` and quote end nothing.
        let code = "jupyter.sql`select ${ x ? `}` : `it's` } from t`; const z = 1;";
        let html = highlight(code, "ts");
        assert_eq!(text(&html), code);
        assert_eq!(
            spans(&html),
            [
                span("hl-s", "`"),
                span("hl-k", "select"),
                span("hl-s", "`}`"),
                span("hl-s", "`it's`"),
                span("hl-k", "from"),
                span("hl-s", "`"),
                span("hl-k", "const"),
                span("hl-n", "1"),
            ]
        );
    }

    #[test]
    fn sql_comments_carry_across_interpolations() {
        let html = highlight("jupyter.sql`-- c ${y} from\nselect 1`", "ts");
        assert_eq!(
            spans(&html),
            [
                span("hl-s", "`"),
                span("hl-c", "-- c "),
                span("hl-c", " from"),
                span("hl-k", "select"),
                span("hl-n", "1"),
                span("hl-s", "`"),
            ]
        );
        let html = highlight("jupyter.sql`/* ${z} from */ select 1`", "ts");
        assert_eq!(
            spans(&html),
            [
                span("hl-s", "`"),
                span("hl-c", "/* "),
                span("hl-c", " from */"),
                span("hl-k", "select"),
                span("hl-n", "1"),
                span("hl-s", "`"),
            ]
        );
    }

    #[test]
    fn sql_template_backslash_is_not_an_sql_escape() {
        // `\'` reaches DuckDB as `'`: it closes the string after `x`, and
        // the last `'` opens another.
        let html = highlight("jupyter.sql`select 'x\\'${q} from'`; const y = 1;", "ts");
        assert_eq!(
            spans(&html),
            [
                span("hl-s", "`"),
                span("hl-k", "select"),
                span("hl-s", "'x\\'"),
                span("hl-k", "from"),
                span("hl-s", "'"),
                span("hl-s", "`"),
                span("hl-k", "const"),
                span("hl-n", "1"),
            ]
        );
        // `\'` opens and closes a string too.
        let html = highlight("jupyter.sql`select \\'from\\' from t`", "ts");
        assert_eq!(
            spans(&html),
            [
                span("hl-s", "`"),
                span("hl-k", "select"),
                span("hl-s", "\\'from\\'"),
                span("hl-k", "from"),
                span("hl-s", "`"),
            ]
        );
        // `\\` is one character and no quote: the string opens at `'a'`,
        // and in a string `\\` leaves the closing quote alone.
        let html = highlight("jupyter.sql`select \\\\'a\\\\' from t`", "ts");
        assert_eq!(
            spans(&html),
            [
                span("hl-s", "`"),
                span("hl-k", "select"),
                span("hl-s", "'a\\\\'"),
                span("hl-k", "from"),
                span("hl-s", "`"),
            ]
        );
    }

    #[test]
    fn sql_quoted_name_is_plain() {
        let html = highlight("jupyter.sql`select \"from\"`", "ts");
        assert_eq!(
            spans(&html),
            [span("hl-s", "`"), span("hl-k", "select"), span("hl-s", "`"),]
        );
    }

    #[test]
    fn sql_template_ends_at_the_real_backtick() {
        // An escaped backtick, even in a string, isn't the end.
        let html = highlight("jupyter.sql`select '\\`' from t`; const x = 1;", "ts");
        assert_eq!(
            spans(&html),
            [
                span("hl-s", "`"),
                span("hl-k", "select"),
                span("hl-s", "'\\`'"),
                span("hl-k", "from"),
                span("hl-s", "`"),
                span("hl-k", "const"),
                span("hl-n", "1"),
            ]
        );
        // A block comment in an interpolation can hold a `}`.
        let html = highlight("jupyter.sql`select ${a /* } */} from t`", "ts");
        assert_eq!(
            spans(&html),
            [
                span("hl-s", "`"),
                span("hl-k", "select"),
                span("hl-c", "/* } */"),
                span("hl-k", "from"),
                span("hl-s", "`"),
            ]
        );
    }

    #[test]
    fn failed_scans_do_not_repeat() {
        // Each tag's interpolation has an unclosed comment, so its scan
        // reaches the end of the cell. If every tag rescanned it, this
        // would take minutes.
        let s = "jupyter.sql`${/*`;".repeat(20_000);
        assert_eq!(text(&highlight(&s, "ts")), s);
    }

    #[test]
    fn failed_scan_turns_sql_off_for_the_cell() {
        // After a template whose end can't be found, a later well-formed
        // `jupyter.sql` template in the same cell stays a plain string.
        let code = "jupyter.sql`${/*`;\njupyter.sql`select 1`";
        let html = highlight(code, "ts");
        assert_eq!(text(&html), code);
        assert!(
            html.contains("<span class=\"hl-s\">`select 1`</span>"),
            "{html}"
        );
        // The same template on its own is SQL.
        let alone = highlight("jupyter.sql`select 1`", "ts");
        assert!(alone.contains("hl-k\">select"), "{alone}");
    }

    #[test]
    fn sql_tag_must_stand_alone() {
        let code = "x.jupyter.sql`select 1`; this.jupyter.sql.query(`select 1`);";
        let html = highlight(code, "ts");
        assert_eq!(text(&html), code);
        assert!(!html.contains("hl-k\">select"), "{html}");
        assert_eq!(
            html.matches("<span class=\"hl-s\">`select 1`</span>")
                .count(),
            2,
            "{html}"
        );
    }

    #[test]
    fn sql_tag_in_a_comment_tags_nothing() {
        let code = "// see jupyter.sql\n`select 1`;\n// jupyter.sql.query(\n`select 2`; jupyter.sql`select 3`";
        let html = highlight(code, "ts");
        assert_eq!(text(&html), code);
        assert_eq!(
            spans(&html),
            [
                span("hl-c", "// see jupyter.sql"),
                span("hl-s", "`select 1`"),
                span("hl-c", "// jupyter.sql.query("),
                span("hl-s", "`select 2`"),
                span("hl-s", "`"),
                span("hl-k", "select"),
                span("hl-n", "3"),
                span("hl-s", "`"),
            ]
        );
    }

    #[test]
    fn sql_template_with_unreadable_interpolation_is_a_string() {
        // The `'` in the regex starts a string the scan can't end, so the
        // template's end isn't found: it renders as before SQL, one string
        // up to its backtick, and the lines after it are untouched.
        let first = "const r = await jupyter.sql`select * from t where name = '${name.replace(/'/g, \"''\")}'`;";
        let rest = "const x = `select from where`;\nfor (const r of rows) console.log(r);";
        let code = format!("{first}\n{rest}");
        let html = highlight(&code, "ts");
        assert_eq!(text(&html), code);
        assert_eq!(
            html,
            format!("{}\n{}", highlight(first, "ts"), highlight(rest, "ts"))
        );
        assert_eq!(
            spans(&highlight(first, "ts"))[..3],
            [
                span("hl-k", "const"),
                span("hl-k", "await"),
                span(
                    "hl-s",
                    "`select * from t where name = '${name.replace(/'/g, \"''\")}'`"
                ),
            ]
        );
        assert!(!html.contains("hl-k\">select"), "{html}");
        // An unclosed template is a string to the line's end, as on main.
        let html = highlight("jupyter.sql`select ${x\nconst y = 1;", "ts");
        assert_eq!(
            spans(&html),
            [
                span("hl-s", "`select ${x"),
                span("hl-k", "const"),
                span("hl-n", "1")
            ]
        );
    }

    #[test]
    fn deep_nesting_is_cut_off() {
        for s in [
            format!("jupyter.sql`{}", "${`".repeat(100_000)),
            format!("jupyter.sql`{}", "${jupyter.sql`".repeat(100_000)),
            format!("jupyter.sql`{}", "${(".repeat(100_000)),
        ] {
            assert_eq!(text(&highlight(&s, "ts")), s);
        }
        // Nesting within the cut-off still highlights.
        let code = "jupyter.sql`select ${`a${`b`}`}`; const x = 1;";
        assert!(highlight(code, "ts").contains("hl-k\">const"));
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
