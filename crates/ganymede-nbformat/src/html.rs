//! Exporting a notebook as one self-contained HTML page to read and share:
//! nbconvert's `html` format, with its own design rather than nbconvert's
//! templates.
//!
//! The page holds the notebook's content and nothing else: cell sources
//! and outputs, never cell or notebook metadata beyond the kernel's
//! display name and, for a journal, who kept it. It makes no network
//! requests and runs no script from the notebook:
//!
//! - Markdown is rendered with raw HTML escaped, links limited to
//!   `http`, `https`, `mailto` and relative targets, and images limited
//!   to the notebook's own (`data:` URIs, attachments); any other image
//!   becomes a link.
//! - SVG and bitmap outputs go in as `<img>` with a `data:` URI, so an
//!   SVG's scripts don't run.
//! - `text/html` outputs go in `<iframe sandbox srcdoc>` frames without
//!   `allow-scripts`. The page's one script sizes those frames.
//! - Outputs that need JavaScript to show (Vega-Lite, widgets) fall back
//!   to another entry in their bundle, or to a short note.
//!
//! [`HtmlOptions::redact`] replaces text the caller knows to be private,
//! such as the server's root directory, wherever it appears.

use crate::export::decode_base64;
use crate::{Cell, Export, MimeBundle, Notebook, Output};
use base64::Engine as _;
use pulldown_cmark::{CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde_json::{Map, Value};
use std::borrow::Cow;
use std::collections::{BTreeSet, HashSet};

/// How code cells show in the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CodeMode {
    /// Each cell's code above its output.
    Shown,
    /// Each cell's code folded above its output, opened with a click.
    #[default]
    Folded,
    /// No code; code cells without output are left out.
    Hidden,
}

impl std::str::FromStr for CodeMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "shown" => Ok(CodeMode::Shown),
            "folded" => Ok(CodeMode::Folded),
            "hidden" => Ok(CodeMode::Hidden),
            other => Err(format!(
                "code must be shown, folded or hidden, not {other:?}"
            )),
        }
    }
}

/// How a page [`to_html`] made begins its generator tag, so a tool can
/// tell such a page from another HTML file before replacing it.
pub const HTML_GENERATOR_MARK: &str = "<meta name=\"generator\" content=\"Ganymede ";

/// Options for [`to_html`].
#[derive(Debug, Clone, Default)]
pub struct HtmlOptions {
    pub code: CodeMode,
    /// A line under the title, such as the date it was published.
    pub byline: Option<String>,
    /// `(text, replacement)` pairs: every occurrence of `text` in the
    /// notebook's sources and outputs is replaced before it goes in the
    /// page. Longer texts are replaced first, so a path under the home
    /// directory is matched before the home directory. Empty texts are
    /// ignored.
    pub redact: Vec<(String, String)>,
}

/// The notebook as one HTML page. `name` is the notebook's file name
/// without `.ipynb`, the title when the notebook has no `# ` heading at
/// the top.
pub fn to_html(nb: &Notebook, name: &str, opts: &HtmlOptions) -> Export {
    let mut page = Page::new(nb, opts);
    let title = page.body(nb, name);
    let body = page.finish(nb, &title, name);
    Export {
        body,
        extension: ".html".to_owned(),
        mimetype: "text/html".to_owned(),
        resources: Vec::new(),
        skipped: page.skipped,
    }
}

/// Output MIME types in the order the page prefers them. Unlike the
/// Markdown export, images come before HTML: an image shows the same in
/// every reader and needs no frame.
const PRIORITY: &[&str] = &[
    "image/svg+xml",
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "text/html",
    "text/markdown",
    "text/latex",
    "text/plain",
];

struct Page<'a> {
    opts: &'a HtmlOptions,
    /// Redactions, longest first.
    redact: Vec<(&'a str, &'a str)>,
    /// Whether any cell carries agent provenance: then cells without it
    /// were written by a person, and are marked.
    journal: bool,
    /// Whether the page marks any cell as a person's.
    person: bool,
    language: String,
    out: String,
    toc: Vec<(String, String)>,
    slugs: HashSet<String>,
    skipped: Vec<String>,
}

impl<'a> Page<'a> {
    fn new(nb: &Notebook, opts: &'a HtmlOptions) -> Self {
        let mut redact: Vec<(&str, &str)> = opts
            .redact
            .iter()
            .filter(|(t, _)| !is_root_like(t))
            .map(|(t, r)| (t.as_str(), r.as_str()))
            .collect();
        redact.sort_by_key(|(t, _)| std::cmp::Reverse(t.len()));
        let language = nb
            .metadata
            .get("language_info")
            .and_then(|li| li.get("name"))
            .or_else(|| {
                nb.metadata
                    .get("kernelspec")
                    .and_then(|k| k.get("language"))
            })
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned();
        Page {
            opts,
            redact,
            journal: nb.cells.iter().any(|c| agent(c).is_some()),
            person: false,
            language,
            out: String::new(),
            toc: Vec::new(),
            slugs: HashSet::new(),
            skipped: Vec::new(),
        }
    }

    /// Text with the redactions applied.
    fn r<'s>(&self, s: &'s str) -> Cow<'s, str> {
        let mut out = Cow::Borrowed(s);
        for (text, with) in &self.redact {
            if out.contains(text) {
                out = Cow::Owned(replace_bounded(&out, text, with));
            }
        }
        out
    }

    /// Renders the cells into `out` and returns the page title.
    fn body(&mut self, nb: &Notebook, name: &str) -> String {
        let mut title = None;
        for (index, cell) in nb.cells.iter().enumerate() {
            match cell {
                Cell::Markdown(c) => {
                    let mut source = self.r(c.source.as_str()).into_owned();
                    if title.is_none() && index == 0 {
                        if let Some((t, rest)) = split_title(&source) {
                            title = Some(t);
                            source = rest;
                        }
                    }
                    for (file, bundle) in c.attachments.iter().flatten() {
                        if let Some(uri) = self.attachment_uri(index, file, bundle) {
                            source = crate::replace_attachment_refs(&source, file, &uri);
                        }
                    }
                    if source.trim().is_empty() {
                        continue;
                    }
                    let html = self.markdown(&source, true);
                    // The title cell's rest is the notebook's own lead,
                    // whoever made the cell.
                    let class = if index == 0 && title.is_some() {
                        "cell note".to_owned()
                    } else {
                        self.cell_class("note", cell)
                    };
                    self.out
                        .push_str(&format!("<section class=\"{class}\">{html}</section>\n"));
                }
                Cell::Raw(c) => {
                    let mime = raw_mimetype(&c.metadata);
                    let source = self.r(c.source.as_str()).into_owned();
                    if source.trim().is_empty() {
                        continue;
                    }
                    let html = match mime.as_str() {
                        "text/markdown" => self.markdown(&source, true),
                        "text/html" => frame(&source),
                        "" | "text/plain" => pre("raw", &source),
                        _ => continue,
                    };
                    let class = self.cell_class("note", cell);
                    self.out
                        .push_str(&format!("<section class=\"{class}\">{html}</section>\n"));
                }
                Cell::Code(c) => {
                    let source = self.r(c.source.as_str()).into_owned();
                    let mut outputs = String::new();
                    for (o, output) in c.outputs.iter().enumerate() {
                        outputs.push_str(&self.output(index, o, output));
                    }
                    let code = source.trim_end_matches(['\n', '\r']);
                    let has_code = !code.trim().is_empty();
                    let code_html = match (self.opts.code, has_code) {
                        (_, false) | (CodeMode::Hidden, _) => String::new(),
                        (CodeMode::Shown, true) => self.code(code),
                        (CodeMode::Folded, true) => format!(
                            "<details class=\"code\"><summary>{}</summary>{}</details>",
                            esc(&self.code_label()),
                            self.code(code)
                        ),
                    };
                    if code_html.is_empty() && outputs.is_empty() {
                        continue;
                    }
                    let class = self.cell_class("run", cell);
                    self.out.push_str(&format!(
                        "<section class=\"{class}\">{code_html}{}</section>\n",
                        if outputs.is_empty() {
                            String::new()
                        } else {
                            format!("<div class=\"outputs\">{outputs}</div>")
                        }
                    ));
                }
                Cell::Unknown(_) => {}
            }
        }
        title.unwrap_or_else(|| self.r(name).into_owned())
    }

    fn code_label(&self) -> String {
        if self.language.is_empty() {
            "Code".to_owned()
        } else {
            format!("Code · {}", language_label(&self.language))
        }
    }

    fn code(&self, code: &str) -> String {
        format!("<pre class=\"source\"><code>{}</code></pre>", esc(code))
    }

    /// `cell <kind>`, plus `by-person` for a journal cell a person wrote
    /// and `recorded` for a step that ran outside the kernel.
    fn cell_class(&mut self, kind: &str, cell: &Cell) -> String {
        let mut class = format!("cell {kind}");
        match agent(cell) {
            Some(a) if a.get("mode").and_then(Value::as_str) == Some("recorded") => {
                class.push_str(" recorded")
            }
            Some(_) => {}
            None if self.journal => {
                self.person = true;
                class.push_str(" by-person")
            }
            None => {}
        }
        class
    }

    /// An attachment's first image entry as a `data:` URI.
    fn attachment_uri(&mut self, cell: usize, file: &str, bundle: &MimeBundle) -> Option<String> {
        bundle.iter().find_map(|(mime, value)| {
            let text = value.as_str()?;
            match mime.as_str() {
                "image/svg+xml" => Some(svg_uri(&self.r(text))),
                m if is_bitmap(m) => match decode_base64(text) {
                    Some(bytes) => Some(bitmap_uri(m, &bytes)),
                    None => {
                        self.skipped
                            .push(format!("cell {cell}, attachment {file}: {m}"));
                        None
                    }
                },
                _ => None,
            }
        })
    }

    fn output(&mut self, cell: usize, index: usize, output: &Output) -> String {
        let data = match output {
            Output::Stream(s) => {
                let text = crate::overwrite_returns(&crate::strip_ansi(s.text.as_str()));
                let text = self.r(text.trim_end()).into_owned();
                if text.trim().is_empty() {
                    return String::new();
                }
                let class = if s.name == "stderr" {
                    "stream stderr"
                } else {
                    "stream"
                };
                return pre(class, &text);
            }
            Output::Error(e) => {
                let lines: Vec<String> = e.traceback.iter().map(|l| crate::strip_ansi(l)).collect();
                let text = if lines.is_empty() {
                    format!("{}: {}", e.ename, e.evalue)
                } else {
                    lines.join("\n")
                };
                let text = self.r(text.trim_end()).into_owned();
                return pre("error", &text);
            }
            Output::DisplayData(d) => &d.data,
            Output::ExecuteResult(r) => &r.data,
            Output::Unknown(_) => return String::new(),
        };
        let alt = data
            .get_str("text/plain")
            .map(|t| self.r(t.trim()).into_owned())
            .filter(|t| !t.is_empty() && t.chars().count() <= 200)
            .unwrap_or_else(|| "Output image".to_owned());
        for mime in PRIORITY {
            let Some(value) = data.get_str(mime) else {
                continue;
            };
            match *mime {
                "image/svg+xml" => {
                    return figure(&svg_uri(&self.r(value)), &alt);
                }
                m if is_bitmap(m) => match decode_base64(value) {
                    Some(bytes) => return figure(&bitmap_uri(m, &bytes), &alt),
                    None => {
                        self.skipped
                            .push(format!("cell {cell}, output {index}: {m}"));
                        continue;
                    }
                },
                "text/html" => {
                    let html = self.r(value);
                    if html.trim().is_empty() {
                        return String::new();
                    }
                    return format!("<div class=\"html\">{}</div>", frame(&html));
                }
                "text/markdown" => {
                    let md = self.r(value).into_owned();
                    if md.trim().is_empty() {
                        return String::new();
                    }
                    return format!(
                        "<div class=\"rendered\">{}</div>",
                        self.markdown(&md, false)
                    );
                }
                _ => {
                    let text = self.r(&crate::strip_ansi(value)).trim_end().to_owned();
                    if text.trim().is_empty() {
                        return String::new();
                    }
                    return pre("text", &text);
                }
            }
        }
        // Nothing the page can show without running the output's code.
        let kinds: BTreeSet<&str> = data.iter().map(|(m, _)| m.as_str()).collect();
        if kinds.is_empty() {
            return String::new();
        }
        let kinds: Vec<&str> = kinds.into_iter().collect();
        format!(
            "<p class=\"left-out\">An interactive output ({}) is left out: it needs the notebook to show.</p>",
            esc(&kinds.join(", "))
        )
    }

    /// Markdown as HTML: raw HTML escaped, links and images checked, and,
    /// for cells (`sections`), headings given ids and listed in the
    /// contents. Headings start at `<h2>`, under the page title.
    fn markdown(&mut self, source: &str, sections: bool) -> String {
        let options =
            Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
        let mut events: Vec<Event> = Vec::new();
        // For each open image: whether it was turned into a link.
        let mut images: Vec<bool> = Vec::new();
        let mut heading: Option<(usize, String)> = None;
        for event in Parser::new_ext(source, options) {
            let event = match event {
                Event::Html(h) | Event::InlineHtml(h) => match inline_img(&h) {
                    Some(img) => Event::InlineHtml(CowStr::from(img)),
                    None => Event::Text(h),
                },
                Event::Start(Tag::Image {
                    link_type,
                    dest_url,
                    title,
                    id,
                }) => {
                    if is_inline_image(&dest_url) {
                        images.push(false);
                        Event::Start(Tag::Image {
                            link_type,
                            dest_url,
                            title,
                            id,
                        })
                    } else {
                        images.push(true);
                        Event::Start(Tag::Link {
                            link_type,
                            dest_url: safe_link(dest_url),
                            title,
                            id,
                        })
                    }
                }
                Event::End(TagEnd::Image) => {
                    if images.pop().unwrap_or(false) {
                        Event::End(TagEnd::Link)
                    } else {
                        Event::End(TagEnd::Image)
                    }
                }
                Event::Start(Tag::Link {
                    link_type,
                    dest_url,
                    title,
                    id,
                }) => Event::Start(Tag::Link {
                    link_type,
                    dest_url: safe_link(dest_url),
                    title,
                    id,
                }),
                Event::Start(Tag::Heading { level, .. }) => {
                    heading = Some((events.len(), String::new()));
                    Event::Start(Tag::Heading {
                        level: level.max(HeadingLevel::H2),
                        id: None,
                        classes: Vec::new(),
                        attrs: Vec::new(),
                    })
                }
                Event::End(TagEnd::Heading(level)) => {
                    if let Some((at, text)) = heading.take() {
                        let level = level.max(HeadingLevel::H2);
                        if sections {
                            let slug = self.slug(&text);
                            if level == HeadingLevel::H2 {
                                self.toc.push((slug.clone(), text.trim().to_owned()));
                            }
                            events[at] = Event::Start(Tag::Heading {
                                level,
                                id: Some(CowStr::from(slug)),
                                classes: Vec::new(),
                                attrs: Vec::new(),
                            });
                        }
                        Event::End(TagEnd::Heading(level))
                    } else {
                        Event::End(TagEnd::Heading(level))
                    }
                }
                other => other,
            };
            if let (Some((_, text)), Event::Text(t) | Event::Code(t)) = (&mut heading, &event) {
                text.push_str(t);
            }
            events.push(event);
        }
        let mut html = String::new();
        pulldown_cmark::html::push_html(&mut html, events.into_iter());
        html
    }

    /// A unique heading id from its text.
    fn slug(&mut self, text: &str) -> String {
        let mut base = String::from("s-");
        let mut dash = false;
        for c in text.chars().flat_map(char::to_lowercase) {
            if c.is_alphanumeric() {
                base.push(c);
                dash = false;
            } else if !dash && base.len() > 2 {
                base.push('-');
                dash = true;
            }
        }
        let base = base.trim_end_matches('-').to_owned();
        let mut slug = base.clone();
        let mut n = 2;
        while !self.slugs.insert(slug.clone()) {
            slug = format!("{base}-{n}");
            n += 1;
        }
        slug
    }

    /// The whole page around the rendered cells.
    fn finish(&self, nb: &Notebook, title: &str, name: &str) -> String {
        let kernel = nb
            .metadata
            .get("kernelspec")
            .and_then(|k| k.get("display_name"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let mut kept_by: Vec<String> = nb
            .cells
            .iter()
            .filter_map(|c| agent(c)?.get("by")?.as_str().map(str::to_owned))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let person = self.person;
        let mut meta: Vec<String> = Vec::new();
        if let Some(byline) = &self.opts.byline {
            meta.push(esc(byline));
        }
        if !kept_by.is_empty() {
            if person {
                kept_by.push("a person".to_owned());
            }
            let who = match kept_by.len() {
                1 => kept_by[0].clone(),
                n => format!("{} and {}", kept_by[..n - 1].join(", "), kept_by[n - 1]),
            };
            meta.push(format!("Journal kept by {}", esc(&who)));
        }
        if let Some(k) = kernel {
            meta.push(esc(k));
        }
        let meta = if meta.is_empty() {
            String::new()
        } else {
            format!(
                "<p class=\"meta\">{}</p>",
                meta.join("<span class=\"dot\" aria-hidden=\"true\">·</span>")
            )
        };
        let legend = if person {
            "<p class=\"legend\"><span class=\"swatch\" aria-hidden=\"true\"></span>Marked cells were written by a person; the rest by the agent.</p>"
        } else {
            ""
        };
        let toc = if self.toc.len() >= 2 {
            let items: String = self
                .toc
                .iter()
                .map(|(id, text)| format!("<li><a href=\"#{id}\">{}</a></li>", esc(text)))
                .collect();
            format!(
                "<nav class=\"toc\" aria-label=\"Contents\"><p class=\"toc-title\">Contents</p><ol>{items}</ol></nav>"
            )
        } else {
            String::new()
        };
        let file = format!("{}.ipynb", self.r(name));
        format!(
            "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
             {HTML_GENERATOR_MARK}{version}\">\n\
             <title>{title_text}</title>\n<style>{CSS}</style>\n</head>\n<body>\n\
             <div class=\"layout{has_toc}\">\n{toc}\n<main>\n<header class=\"masthead\">\n\
             <h1>{title_html}</h1>\n{meta}\n{legend}\n</header>\n<article>\n{body}</article>\n\
             <footer class=\"colophon\">Exported from <span class=\"file\">{file}</span> by Ganymede.</footer>\n\
             </main>\n</div>\n<script>{SCRIPT}</script>\n</body>\n</html>\n",
            version = env!("CARGO_PKG_VERSION"),
            title_text = esc(&plain_title(title)),
            title_html = self.inline_markdown(title),
            has_toc = if toc.is_empty() { "" } else { " with-toc" },
            body = self.out,
            file = esc(&file),
        )
    }

    /// One line of Markdown as inline HTML, for the title.
    fn inline_markdown(&self, text: &str) -> String {
        let text = escape_block_start(text);
        let events = Parser::new(&text).filter_map(|e| match e {
            Event::Text(t) => Some(Event::Text(t)),
            Event::Code(t) => Some(Event::Code(t)),
            Event::Html(h) | Event::InlineHtml(h) => Some(Event::Text(h)),
            Event::Start(t @ (Tag::Emphasis | Tag::Strong)) => Some(Event::Start(t)),
            Event::End(t @ (TagEnd::Emphasis | TagEnd::Strong)) => Some(Event::End(t)),
            _ => None,
        });
        let mut html = String::new();
        pulldown_cmark::html::push_html(&mut html, events);
        html
    }
}

/// The cell's `metadata.ganymede.agent`, set on cells an agent wrote.
fn agent(cell: &Cell) -> Option<&Map<String, Value>> {
    let metadata = match cell {
        Cell::Code(c) => &c.metadata,
        Cell::Markdown(c) => &c.metadata,
        Cell::Raw(c) => &c.metadata,
        Cell::Unknown(_) => return None,
    };
    metadata.get("ganymede")?.get("agent")?.as_object()
}

/// Whether a redaction would rewrite text it shouldn't: empty, or a
/// filesystem root such as `/`, `C:\\` or `\\`, which every path contains.
fn is_root_like(text: &str) -> bool {
    let trimmed = text.trim_end_matches(['/', '\\']);
    trimmed.is_empty() || (trimmed.len() == 2 && trimmed.ends_with(':'))
}

/// Whether `c` can continue a file or directory name, so a match that
/// it follows or precedes is part of a longer name.
fn name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-')
}

/// `text` replaced by `with` wherever it stands on its own: not inside a
/// longer name (`/home/tom` in `/home/tommy`, `/srv/nb` in
/// `/srv/nb-old`). A `.` after it ends the match only when no name
/// character follows the dot, so a sentence can end on the path but
/// `/srv/nb.bak` is left alone.
fn replace_bounded(s: &str, text: &str, with: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    let mut prev: Option<char> = None;
    while let Some(at) = rest.find(text) {
        let before = rest[..at]
            .chars()
            .next_back()
            .or(if at == 0 { prev } else { None });
        let after = &rest[at + text.len()..];
        let mut next = after.chars();
        let ends = match next.next() {
            None => true,
            Some('.') => !next.next().is_some_and(name_char),
            Some(c) => !name_char(c),
        };
        let starts = !before.is_some_and(name_char) || text.starts_with(|c: char| !name_char(c));
        out.push_str(&rest[..at]);
        if ends && starts {
            out.push_str(with);
        } else {
            out.push_str(text);
        }
        prev = text.chars().next_back();
        rest = after;
    }
    out.push_str(rest);
    out
}

/// A raw cell's target MIME type.
fn raw_mimetype(metadata: &Map<String, Value>) -> String {
    metadata
        .get("raw_mimetype")
        .or_else(|| metadata.get("format"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// A leading `# Title` line split from the rest of the cell.
fn split_title(source: &str) -> Option<(String, String)> {
    let trimmed = source.trim_start();
    let (first, rest) = trimmed.split_once('\n').unwrap_or((trimmed, ""));
    let mut title = first.strip_prefix("# ")?.trim();
    // A closing run of `#` goes only after a space, as CommonMark's.
    let closed = title.trim_end_matches('#');
    if closed.len() < title.len() && (closed.is_empty() || closed.ends_with(' ')) {
        title = closed.trim_end();
    }
    (!title.is_empty()).then(|| (title.to_owned(), rest.to_owned()))
}

/// A line with what would start a block (`1. `, `- `, `> `, `#`) escaped,
/// so it parses as one inline paragraph.
fn escape_block_start(text: &str) -> String {
    let digits = text.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && text[digits..].starts_with(['.', ')']) {
        return format!("{}\\{}", &text[..digits], &text[digits..]);
    }
    if text.starts_with(['-', '+', '*', '>', '#', '=', '|']) {
        return format!("\\{text}");
    }
    text.to_owned()
}

/// The title as plain text, for `<title>`.
fn plain_title(title: &str) -> String {
    title.replace(['*', '`'], "").replace("__", "")
}

fn language_label(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "typescript" => "TypeScript".to_owned(),
        "javascript" => "JavaScript".to_owned(),
        "python" => "Python".to_owned(),
        _ => name.to_owned(),
    }
}

fn is_bitmap(mime: &str) -> bool {
    matches!(
        mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}

fn svg_uri(svg: &str) -> String {
    format!(
        "data:image/svg+xml;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(standalone_svg(svg).as_bytes())
    )
}

/// SVG as a file needs it: kernels serialize SVG from an HTML DOM (as
/// Observable Plot's does), which leaves out the namespace an `<img>`
/// needs to draw it.
fn standalone_svg(svg: &str) -> Cow<'_, str> {
    let Some(start) = svg.find("<svg") else {
        return Cow::Borrowed(svg);
    };
    let Some(end) = svg[start..].find('>').map(|e| start + e) else {
        return Cow::Borrowed(svg);
    };
    let tag = &svg[start..end];
    let mut add = String::new();
    if !tag.contains("xmlns=") {
        add.push_str(" xmlns=\"http://www.w3.org/2000/svg\"");
    }
    if !tag.contains("xmlns:xlink=") && svg.contains("xlink:") {
        add.push_str(" xmlns:xlink=\"http://www.w3.org/1999/xlink\"");
    }
    if add.is_empty() {
        return Cow::Borrowed(svg);
    }
    let at = start + "<svg".len();
    Cow::Owned(format!("{}{add}{}", &svg[..at], &svg[at..]))
}

/// Bytes decoded from the notebook and encoded again, so nothing but
/// base64 reaches the attribute.
fn bitmap_uri(mime: &str, bytes: &[u8]) -> String {
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

fn figure(uri: &str, alt: &str) -> String {
    format!(
        "<figure class=\"image\"><img src=\"{}\" alt=\"{}\" loading=\"lazy\"></figure>",
        esc(uri),
        esc(alt)
    )
}

fn pre(class: &str, text: &str) -> String {
    format!("<pre class=\"{class}\">{}</pre>", esc(text))
}

/// HTML in a frame that can't run scripts, submit forms or navigate the
/// page. `allow-same-origin` lets the page's script read its height; the
/// frame runs no script of its own to use it.
fn frame(html: &str) -> String {
    let doc = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" content=\"{FRAME_CSP}\"><style>{FRAME_CSS}</style></head><body>{html}</body></html>"
    );
    format!(
        "<iframe class=\"frame\" sandbox=\"allow-same-origin\" referrerpolicy=\"no-referrer\" title=\"Output\" srcdoc=\"{}\"></iframe>",
        esc(&doc)
    )
}

/// Whether a Markdown image stays an image: only the notebook's own
/// (`data:` image URIs), so the page fetches nothing.
fn is_inline_image(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    [
        "data:image/png",
        "data:image/jpeg",
        "data:image/gif",
        "data:image/webp",
        "data:image/svg+xml",
    ]
    .iter()
    .any(|p| {
        lower
            .strip_prefix(p)
            .is_some_and(|r| r.starts_with([';', ',']))
    })
}

/// A raw `<img>` tag whose `src` is an inline image (a cell attachment,
/// after its `attachment:` link was pointed at its data), as a tag of
/// the page's own making: only `src` and `alt` are kept. Any other raw
/// HTML is shown as text.
fn inline_img(html: &str) -> Option<String> {
    let tag = html.trim();
    if !tag.get(..4)?.eq_ignore_ascii_case("<img") || !tag.ends_with('>') || tag[1..].contains('<')
    {
        return None;
    }
    let attr = |name: &str| -> Option<String> {
        let lower = tag.to_ascii_lowercase();
        let mut from = 0;
        while let Some(i) = lower[from..].find(name) {
            let at = from + i;
            from = at + name.len();
            if !lower[..at].ends_with(char::is_whitespace) {
                continue;
            }
            let rest = tag[at + name.len()..]
                .trim_start()
                .strip_prefix('=')?
                .trim_start();
            let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
            let value = &rest[1..];
            return Some(value[..value.find(quote)?].to_owned());
        }
        None
    };
    let src = attr("src")?;
    if !is_inline_image(&src) {
        return None;
    }
    let alt = attr("alt").unwrap_or_default();
    Some(format!("<img src=\"{}\" alt=\"{}\">", esc(&src), esc(&alt)))
}

/// A link target, or `#` when its scheme could run code (`javascript:`)
/// or isn't one a reader follows.
fn safe_link(url: CowStr<'_>) -> CowStr<'_> {
    let lower = url.trim().to_ascii_lowercase();
    let scheme_end = lower.find(':');
    let path_start = lower.find(['/', '?', '#']);
    let has_scheme = match (scheme_end, path_start) {
        (Some(c), Some(p)) => c < p,
        (Some(_), None) => true,
        _ => false,
    };
    if !has_scheme
        || lower.starts_with("http:")
        || lower.starts_with("https:")
        || lower.starts_with("mailto:")
    {
        url
    } else {
        CowStr::from("#")
    }
}

/// `text` escaped for HTML text and double-quoted attributes.
fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// The policy inside output frames, set before the output so a policy the
/// output adds can only narrow it: nothing loads from anywhere but the
/// frame's own `data:` images and inline styles, and no form submits.
const FRAME_CSP: &str = "default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; form-action 'none'";

/// Sizes each output frame to its content.
const SCRIPT: &str = "for(const f of document.querySelectorAll('iframe.frame')){const fit=()=>{try{const d=f.contentDocument;if(d&&d.documentElement){f.style.height=Math.ceil(d.body.getBoundingClientRect().height+4)+'px';}}catch(e){}};f.addEventListener('load',fit);fit();}";

/// The style inside output frames: tables as the page sets them.
const FRAME_CSS: &str = "\
:root{color-scheme:light dark;--fg:#1b1f24;--soft:#4a525c;--line:#dedad0;--alt:#f2f0ea}\
@media (prefers-color-scheme:dark){:root{--fg:#e6e8ea;--soft:#a4adb6;--line:#2a3238;--alt:#171c20}}\
html,body{margin:0;background:transparent;color:var(--fg);font:15px/1.5 system-ui,-apple-system,'Segoe UI',sans-serif}\
body{padding:2px 0;overflow-x:auto}\
table{border-collapse:collapse;font-variant-numeric:tabular-nums;font-size:13.5px}\
th,td{padding:5px 10px;border-bottom:1px solid var(--line);text-align:right}\
th{font-weight:600;color:var(--soft)}\
tr:nth-child(even) td{background:var(--alt)}\
img,svg{max-width:100%;height:auto}";

/// The page's style: the site's palette (warm paper, teal accent), serif
/// prose, a contents column on wide screens, light and dark.
const CSS: &str = r#"
:root {
  --bg: #fbfaf7; --surface: #ffffff; --alt: #f2f0ea; --fg: #1b1f24; --soft: #4a525c;
  --line: #dedad0; --accent: #2e6e64; --accent-soft: #e3efec; --person: #8a5a12;
  --person-soft: #f6ecd9; --error: #a1261d; --error-soft: #fbeceb; --figure: #ffffff;
  --serif: ui-serif, "Iowan Old Style", "Charter", "Source Serif Pro", Georgia, Cambria, serif;
  --sans: system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", sans-serif;
  --mono: ui-monospace, "SF Mono", SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace;
  color-scheme: light;
}
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) {
    --bg: #111518; --surface: #161b1f; --alt: #171c20; --fg: #e6e8ea; --soft: #a4adb6;
    --line: #2a3238; --accent: #5aa89b; --accent-soft: #17302c; --person: #e0ae5c;
    --person-soft: #2b2416; --error: #f2958d; --error-soft: #2c1816; --figure: #f4f3ef;
    color-scheme: dark;
  }
}
:root[data-theme="dark"] {
  --bg: #111518; --surface: #161b1f; --alt: #171c20; --fg: #e6e8ea; --soft: #a4adb6;
  --line: #2a3238; --accent: #5aa89b; --accent-soft: #17302c; --person: #e0ae5c;
  --person-soft: #2b2416; --error: #f2958d; --error-soft: #2c1816; --figure: #f4f3ef;
  color-scheme: dark;
}
* { box-sizing: border-box; }
html { -webkit-text-size-adjust: 100%; scroll-padding-top: 1.5rem; }
body { margin: 0; background: var(--bg); color: var(--fg); font: 18px/1.65 var(--serif); }
.layout { max-width: 48rem; margin: 0 auto; padding: 3.5rem 1rem 4rem; }
.layout.with-toc { max-width: 70rem; display: grid; grid-template-columns: 1fr; }
main { min-width: 0; }
.masthead { margin-bottom: 2.75rem; padding-bottom: 1.75rem; border-bottom: 1px solid var(--line); }
.masthead h1 { font-size: clamp(2rem, 5vw, 2.85rem); line-height: 1.12; margin: 0 0 .9rem; letter-spacing: -.015em; font-weight: 600; text-wrap: balance; }
.meta { font: 500 .78rem/1.5 var(--sans); color: var(--soft); text-transform: uppercase; letter-spacing: .07em; margin: 0; }
.meta .dot { margin: 0 .55em; color: var(--line); }
.legend { font: .85rem/1.5 var(--sans); color: var(--soft); margin: .9rem 0 0; display: flex; align-items: center; gap: .55em; }
.swatch { width: 3px; height: 1.1em; background: var(--person); border-radius: 2px; flex: none; }
.toc { display: none; }
.toc-title { font: 600 .72rem/1 var(--sans); text-transform: uppercase; letter-spacing: .09em; color: var(--soft); margin: 0 0 .9rem; }
.toc ol { list-style: none; margin: 0; padding: 0; font: .85rem/1.4 var(--sans); }
.toc li { margin: 0 0 .6rem; }
.toc a { color: var(--soft); text-decoration: none; }
.toc a:hover { color: var(--accent); }
@media (min-width: 72rem) {
  .layout.with-toc { grid-template-columns: 13rem minmax(0, 46rem); column-gap: 4rem; justify-content: center; }
  .toc { display: block; position: sticky; top: 3.5rem; align-self: start; max-height: calc(100vh - 5rem); overflow: auto; padding-top: .6rem; }
}
article { counter-reset: none; }
.cell { margin: 0 0 1.4rem; }
.cell > :first-child { margin-top: 0; }
.cell > :last-child { margin-bottom: 0; }
h2 { font-size: 1.55rem; line-height: 1.25; margin: 3rem 0 .9rem; font-weight: 600; letter-spacing: -.01em; text-wrap: balance; }
.cell.note:first-child h2 { margin-top: 0; }
h3 { font-size: 1.2rem; margin: 2rem 0 .6rem; font-weight: 600; }
h4, h5, h6 { font: 600 1rem/1.4 var(--sans); margin: 1.6rem 0 .5rem; }
p { margin: 0 0 1rem; }
a { color: var(--accent); text-underline-offset: .18em; text-decoration-thickness: 1px; }
strong { font-weight: 650; }
ul, ol { padding-left: 1.4rem; margin: 0 0 1rem; }
li { margin: .25rem 0; }
li::marker { color: var(--soft); }
blockquote { margin: 0 0 1rem; padding: .1rem 0 .1rem 1.1rem; border-left: 3px solid var(--line); color: var(--soft); }
hr { border: 0; border-top: 1px solid var(--line); margin: 2.5rem 0; }
code { font: .84em/1.5 var(--mono); background: var(--alt); padding: .1em .35em; border-radius: 4px; }
pre { font: 13.5px/1.55 var(--mono); margin: 0 0 1rem; padding: .9rem 1.05rem; background: var(--alt); border-radius: 8px; overflow-x: auto; white-space: pre; tab-size: 4; }
pre code { background: none; padding: 0; font: inherit; }
table { border-collapse: collapse; margin: 0 0 1rem; font: .86rem/1.45 var(--sans); font-variant-numeric: tabular-nums; display: block; overflow-x: auto; max-width: 100%; }
th, td { padding: .42rem .8rem; border-bottom: 1px solid var(--line); text-align: left; vertical-align: top; }
th { font-weight: 600; color: var(--soft); border-bottom-color: var(--soft); white-space: nowrap; }
img { max-width: 100%; height: auto; }
.cell.by-person { position: relative; padding: .9rem 1.1rem; background: var(--person-soft); border-left: 3px solid var(--person); border-radius: 0 8px 8px 0; }
.cell.by-person::before { content: "Person"; display: block; font: 600 .68rem/1 var(--sans); letter-spacing: .09em; text-transform: uppercase; color: var(--person); margin-bottom: .55rem; }
.cell.recorded { font-size: .92em; color: var(--soft); border-left: 2px dashed var(--line); padding-left: 1rem; }
.cell.recorded pre { color: var(--fg); }
.cell.run { margin: 1.6rem 0 2rem; }
details.code { margin: 0 0 .7rem; }
details.code > summary { font: 500 .78rem/1.4 var(--sans); color: var(--soft); cursor: pointer; list-style: none; display: inline-flex; align-items: center; gap: .45em; padding: .25rem .65rem .25rem .5rem; border: 1px solid var(--line); border-radius: 999px; user-select: none; }
details.code > summary::-webkit-details-marker { display: none; }
details.code > summary::before { content: ""; width: .42em; height: .42em; border-right: 1.5px solid currentColor; border-bottom: 1.5px solid currentColor; transform: rotate(-45deg); transition: transform .15s; margin-right: .1em; }
details.code[open] > summary::before { transform: rotate(45deg); }
details.code > summary:hover { color: var(--accent); border-color: var(--accent); }
details.code[open] > summary { margin-bottom: .6rem; }
pre.source { border-left: 3px solid var(--accent); background: var(--surface); border-top: 1px solid var(--line); border-right: 1px solid var(--line); border-bottom: 1px solid var(--line); }
.outputs > * { margin: 0 0 .9rem; }
.outputs > :last-child { margin-bottom: 0; }
figure.image { margin: 0 0 1rem; padding: 1rem; background: var(--figure); border: 1px solid var(--line); border-radius: 10px; text-align: center; }
@media (min-width: 60rem) { .layout:not(.with-toc) figure.image { margin-left: -2rem; margin-right: -2rem; } }
figure.image img { display: block; margin: 0 auto; }
.rendered { font: .95rem/1.6 var(--sans); }
.rendered table { font-size: .84rem; }
.rendered > :last-child { margin-bottom: 0; }
pre.stream, pre.text { background: var(--surface); border: 1px solid var(--line); }
pre.stderr { border-left: 3px solid var(--person); }
pre.error { background: var(--error-soft); color: var(--error); }
.frame { display: block; width: 100%; height: 12rem; border: 0; background: transparent; }
.left-out { font: italic .9rem/1.5 var(--sans); color: var(--soft); }
.colophon { margin-top: 4rem; padding-top: 1.25rem; border-top: 1px solid var(--line); font: .8rem/1.5 var(--sans); color: var(--soft); }
.colophon .file { font-family: var(--mono); font-size: .95em; }
@media (max-width: 40rem) {
  body { font-size: 17px; }
  .layout { padding-top: 2.25rem; }
  h2 { font-size: 1.35rem; margin-top: 2.4rem; }
  pre { font-size: 12.5px; }
  figure.image { padding: .5rem; margin-left: -.25rem; margin-right: -.25rem; }
  .cell.by-person { padding: .75rem .85rem; }
}
@media print {
  details.code { display: none; }
  .toc { display: none !important; }
  .layout.with-toc { display: block; max-width: 48rem; }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn nb(cells: Value) -> Notebook {
        serde_json::from_value(json!({
            "nbformat": 4, "nbformat_minor": 5,
            "metadata": {
                "kernelspec": {"name": "cdp-node", "display_name": "TypeScript (Node)", "language": "typescript"},
                "language_info": {"name": "typescript"},
                "ganymede": {"secret": "metadata-must-not-leak"}
            },
            "cells": cells
        }))
        .unwrap()
    }

    fn agent_meta(mode: &str) -> Value {
        json!({"ganymede": {"agent": {"by": "claude-code", "mode": mode, "at": "2026-11-11T00:00:00Z",
            "history": [{"source": "old text that was replaced"}]}}})
    }

    fn html(nb: &Notebook, opts: &HtmlOptions) -> String {
        to_html(nb, "2026-11-11-quakes", opts).body
    }

    #[test]
    fn title_toc_and_prose() {
        let n = nb(json!([
            {"cell_type": "markdown", "id": "a", "metadata": {}, "source": "# Did the *quakes* stop?\n\nIntro."},
            {"cell_type": "markdown", "id": "b", "metadata": {}, "source": "## 1. Per year\n\nText."},
            {"cell_type": "markdown", "id": "c", "metadata": {}, "source": "## 2. Per year\n\n| a | b |\n|---|---|\n| 1 | 2 |"},
        ]));
        let page = html(&n, &HtmlOptions::default());
        assert!(page.contains("<title>Did the quakes stop?</title>"));
        assert!(page.contains("<h1>Did the <em>quakes</em> stop?</h1>"));
        assert!(page.contains("<p>Intro.</p>"));
        assert!(page.contains("<h2 id=\"s-1-per-year\">1. Per year</h2>"));
        assert!(page.contains("<h2 id=\"s-2-per-year\">2. Per year</h2>"));
        assert!(page.contains("<a href=\"#s-1-per-year\">1. Per year</a>"));
        assert!(page.contains("<table>"));
        assert!(page.contains("TypeScript (Node)"));
        assert!(page.contains("2026-11-11-quakes.ipynb"));
    }

    #[test]
    fn title_falls_back_to_name() {
        let n = nb(json!([{"cell_type": "markdown", "metadata": {}, "source": "Just text"}]));
        assert!(html(&n, &HtmlOptions::default()).contains("<h1>2026-11-11-quakes</h1>"));
    }

    #[test]
    fn code_modes() {
        let cells = json!([
            {"cell_type": "code", "id": "x", "metadata": {}, "execution_count": 1, "source": "const a = 1 < 2;",
             "outputs": [{"output_type": "execute_result", "execution_count": 1, "metadata": {},
                          "data": {"text/plain": "true"}}]},
            {"cell_type": "code", "id": "y", "metadata": {}, "execution_count": 2, "source": "setup()", "outputs": []},
        ]);
        let n = nb(cells);
        let folded = html(&n, &HtmlOptions::default());
        assert!(folded.contains("<details class=\"code\"><summary>Code · TypeScript</summary>"));
        assert!(folded.contains("const a = 1 &lt; 2;"));
        assert!(folded.contains("<pre class=\"text\">true</pre>"));
        let shown = html(
            &n,
            &HtmlOptions {
                code: CodeMode::Shown,
                ..Default::default()
            },
        );
        assert!(!shown.contains("<details class=\"code\">"));
        assert!(shown.contains("setup()"));
        let hidden = html(
            &n,
            &HtmlOptions {
                code: CodeMode::Hidden,
                ..Default::default()
            },
        );
        assert!(!hidden.contains("const a"));
        assert!(!hidden.contains("setup()"));
        assert!(hidden.contains("<pre class=\"text\">true</pre>"));
        assert_eq!("folded".parse::<CodeMode>(), Ok(CodeMode::Folded));
        assert!("open".parse::<CodeMode>().is_err());
    }

    #[test]
    fn outputs_by_type() {
        let png = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG fake");
        let n = nb(
            json!([{"cell_type": "code", "metadata": {}, "execution_count": 1, "source": "x",
            "outputs": [
                {"output_type": "display_data", "metadata": {},
                 "data": {"image/svg+xml": "<svg onload=\"alert(1)\"></svg>", "text/html": "<div>no</div>", "text/plain": "Plot"}},
                {"output_type": "display_data", "metadata": {}, "data": {"image/png": png, "text/plain": "<Figure>"}},
                {"output_type": "display_data", "metadata": {}, "data": {"image/png": "!!not base64!!", "text/plain": "fallback"}},
                {"output_type": "display_data", "metadata": {}, "data": {"text/html": "<table><tr><td>1</td></tr></table><script>alert(1)</script>"}},
                {"output_type": "display_data", "metadata": {}, "data": {"text/markdown": "**bold** <b>raw</b>"}},
                {"output_type": "display_data", "metadata": {}, "data": {"application/vnd.vegalite.v5+json": {}}},
                {"output_type": "stream", "name": "stderr", "text": "\u{1b}[31mwarn\u{1b}[0m"},
                {"output_type": "error", "ename": "E", "evalue": "v", "traceback": ["\u{1b}[31mTypeError\u{1b}[0m: <bad>"]}
            ]}]),
        );
        let ex = to_html(&n, "n", &HtmlOptions::default());
        let page = ex.body;
        assert!(page.contains("<img src=\"data:image/svg+xml;base64,"));
        assert!(!page.contains("onload=\"alert"));
        assert!(!page.contains("<div>no</div>"));
        assert!(page.contains(&format!("data:image/png;base64,{png}")));
        assert!(page.contains("alt=\"&lt;Figure&gt;\""));
        assert!(page.contains("<pre class=\"text\">fallback</pre>"));
        assert_eq!(ex.skipped, vec!["cell 0, output 2: image/png".to_owned()]);
        // HTML output: escaped into a sandboxed frame without scripts.
        assert!(page.contains("sandbox=\"allow-same-origin\""));
        assert!(!page.contains("allow-scripts"));
        assert!(!page.contains("<script>alert"));
        assert!(page.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(page.contains("<strong>bold</strong> &lt;b&gt;raw&lt;/b&gt;"));
        assert!(page.contains("application/vnd.vegalite.v5+json) is left out"));
        assert!(page.contains("<pre class=\"stream stderr\">warn</pre>"));
        assert!(page.contains("<pre class=\"error\">TypeError: &lt;bad&gt;</pre>"));
        // The page's own script is the only one.
        assert_eq!(page.matches("<script>").count(), 1);
    }

    #[test]
    fn markdown_is_safe() {
        let n = nb(json!([{"cell_type": "markdown", "metadata": {}, "source":
            "<img src=x onerror=alert(1)>\n\n[js](javascript:alert(1)) [web](https://example.com) [rel](data/a.csv)\n\n![remote](https://example.com/x.png) ![](attachment:a.png)",
            "attachments": {"a.png": {"image/png": "iVBORw0KGgo="}}}]));
        let page = html(&n, &HtmlOptions::default());
        assert!(page.contains("&lt;img src=x onerror=alert(1)&gt;"));
        assert!(!page.contains("javascript:"));
        assert!(page.contains("<a href=\"#\">js</a>"));
        assert!(page.contains("<a href=\"https://example.com\">web</a>"));
        assert!(page.contains("<a href=\"data/a.csv\">rel</a>"));
        assert!(page.contains("<a href=\"https://example.com/x.png\">remote</a>"));
        assert!(!page.contains("src=\"https://"));
        assert!(page.contains("<img src=\"data:image/png;base64,iVBORw0KGgo=\""));
    }

    #[test]
    fn journal_marks_and_no_metadata() {
        let n = nb(json!([
            {"cell_type": "markdown", "metadata": {}, "source": "# Title"},
            {"cell_type": "markdown", "metadata": agent_meta("note"), "source": "Agent note."},
            {"cell_type": "markdown", "metadata": agent_meta("recorded"), "source": "Recorded: ran in the shell."},
            {"cell_type": "markdown", "metadata": {"tags": ["mine"]}, "source": "What about 2019?"},
        ]));
        let page = html(
            &n,
            &HtmlOptions {
                byline: Some("11 Nov 2026".into()),
                ..Default::default()
            },
        );
        assert!(page.contains("<section class=\"cell note\"><p>Agent note.</p>"));
        assert!(page.contains("<section class=\"cell note recorded\">"));
        assert!(page.contains("<section class=\"cell note by-person\"><p>What about 2019?</p>"));
        assert!(page.contains("Journal kept by claude-code and a person"));
        assert!(page.contains("11 Nov 2026"));
        assert!(page.contains("Marked cells were written by a person"));
        for leak in [
            "metadata-must-not-leak",
            "old text that was replaced",
            "2026-11-11T00:00:00Z",
            "mine",
        ] {
            assert!(!page.contains(leak), "{leak} leaked");
        }
    }

    #[test]
    fn not_a_journal_marks_nothing() {
        let n = nb(json!([{"cell_type": "markdown", "metadata": {}, "source": "Plain."}]));
        let page = html(&n, &HtmlOptions::default());
        assert!(!page.contains("by-person\""));
        assert!(!page.contains("Journal kept by"));
    }

    #[test]
    fn redacts_everywhere() {
        let svg = "<svg><text>/home/tl/proj/data.csv</text></svg>";
        let n = nb(json!([
            {"cell_type": "markdown", "metadata": {}, "source": "Data in /home/tl/proj/data.csv and /home/tl/other"},
            {"cell_type": "code", "metadata": {}, "execution_count": 1, "source": "read('/home/tl/proj/x') // tok123",
             "outputs": [
                {"output_type": "stream", "name": "stdout", "text": "token=tok123\n"},
                {"output_type": "display_data", "metadata": {}, "data": {"image/svg+xml": svg}},
                {"output_type": "error", "ename": "E", "evalue": "", "traceback": ["at /home/tl/proj/a.ts:1"]}
             ]}
        ]));
        let opts = HtmlOptions {
            code: CodeMode::Shown,
            redact: vec![
                ("/home/tl".into(), "~".into()),
                ("/home/tl/proj".into(), ".".into()),
                ("tok123".into(), "[token]".into()),
                (String::new(), "x".into()),
            ],
            ..Default::default()
        };
        let page = html(&n, &opts);
        assert!(!page.contains("/home/tl"));
        assert!(!page.contains("tok123"));
        assert!(page.contains("./data.csv and ~/other"));
        assert!(page.contains("token=[token]"));
        let b64 = base64::engine::general_purpose::STANDARD
            .encode("<svg xmlns=\"http://www.w3.org/2000/svg\"><text>./data.csv</text></svg>");
        assert!(page.contains(&b64));
    }

    #[test]
    fn svg_gets_its_namespace() {
        assert_eq!(
            standalone_svg("<svg viewBox=\"0 0 1 1\"><use xlink:href=\"#a\"/></svg>"),
            "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" viewBox=\"0 0 1 1\"><use xlink:href=\"#a\"/></svg>"
        );
        let full = "<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
        assert_eq!(standalone_svg(full), full);
    }

    #[test]
    fn html_output_frames_load_nothing() {
        let n = nb(
            json!([{"cell_type": "code", "metadata": {}, "execution_count": 1, "source": "x",
            "outputs": [{"output_type": "display_data", "metadata": {},
                "data": {"text/html": "<img src=\"https://tracker.example/p.gif\">"}}]}]),
        );
        let page = html(&n, &HtmlOptions::default());
        let csp = format!("content=&quot;{FRAME_CSP}&quot;");
        let at_csp = page.find(&csp.replace('\'', "&#39;")).expect("frame CSP");
        let at_output = page.find("tracker.example").unwrap();
        assert!(at_csp < at_output, "the policy comes before the output");
        assert!(FRAME_CSP.starts_with("default-src 'none'; img-src data:;"));
    }

    #[test]
    fn redaction_keeps_to_whole_names() {
        assert_eq!(
            replace_bounded(
                "/home/tom/a /home/tommy/b /home/tom. /home/tom.bak x/home/tom",
                "/home/tom",
                "~"
            ),
            "~/a /home/tommy/b ~. /home/tom.bak x~"
        );
        assert_eq!(
            replace_bounded("tok123 xtok1234", "tok123", "[t]"),
            "[t] xtok1234"
        );
        for root in ["/", "//", "C:\\", "\\", ""] {
            assert!(is_root_like(root), "{root}");
        }
        assert!(!is_root_like("/srv"));
        let n =
            nb(json!([{"cell_type": "markdown", "metadata": {}, "source": "See https://x.org/a"}]));
        let page = html(
            &n,
            &HtmlOptions {
                redact: vec![("/".into(), ".".into())],
                ..Default::default()
            },
        );
        assert!(page.contains("https://x.org/a"));
    }

    #[test]
    fn titles_keep_their_text() {
        for (src, want) in [
            ("# 1. Setup", "<h1>1. Setup</h1>"),
            ("# 2024) Results", "<h1>2024) Results</h1>"),
            ("# - draft", "<h1>- draft</h1>"),
            ("# Intro to C#", "<h1>Intro to C#</h1>"),
            ("# Closed ##", "<h1>Closed</h1>"),
        ] {
            let n = nb(json!([{"cell_type": "markdown", "metadata": {}, "source": src}]));
            let page = html(&n, &HtmlOptions::default());
            assert!(
                page.contains(want),
                "{src}: {}",
                &page[page.find("<h1>").unwrap()..][..60]
            );
        }
    }

    #[test]
    fn raw_img_attachments_and_data_svg() {
        let n = nb(json!([{"cell_type": "markdown", "metadata": {}, "source":
            "<img src=\"attachment:a.png\" width=\"40\" onerror=\"x()\">\n\n![s](data:image/svg+xml,%3Csvg%3E)\n\n[^1] footnote",
            "attachments": {"a.png": {"image/png": "iVBORw0KGgo="}}}]));
        let page = html(&n, &HtmlOptions::default());
        assert!(
            page.contains("<img src=\"data:image/png;base64,iVBORw0KGgo=\" alt=\"\">"),
            "{page}"
        );
        assert!(!page.contains("onerror"));
        assert!(page.contains("<img src=\"data:image/svg+xml,%3Csvg%3E\""));
        assert!(!page.contains("footnote-definition") && !page.contains("id=\"1\""));
    }

    #[test]
    fn person_mark_follows_shown_cells() {
        // An empty person cell and the title cell's own text mark nothing.
        let n = nb(json!([
            {"cell_type": "markdown", "metadata": {}, "source": "# T\n\nLead."},
            {"cell_type": "markdown", "metadata": agent_meta("note"), "source": "Note."},
            {"cell_type": "markdown", "metadata": {}, "source": "  "},
        ]));
        let page = html(&n, &HtmlOptions::default());
        assert!(!page.contains("by-person\""));
        assert!(!page.contains("a person"));
        assert!(page.contains("Journal kept by claude-code<"));
    }

    #[test]
    fn headings_dedupe_and_demote() {
        let n = nb(json!([
            {"cell_type": "markdown", "metadata": {}, "source": "Intro"},
            {"cell_type": "markdown", "metadata": {}, "source": "# Part\n\n## Part"},
        ]));
        let page = html(&n, &HtmlOptions::default());
        assert!(page.contains("<h2 id=\"s-part\">Part</h2>"));
        assert!(page.contains("<h2 id=\"s-part-2\">Part</h2>"));
    }
}
