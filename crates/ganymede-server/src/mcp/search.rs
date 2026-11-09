//! `notebooks_search` (#624): find cells across notebooks, so an agent
//! starting a new journal can see what earlier ones found.
//!
//! A cell matches when every word of the query appears in it, ignoring
//! case: in its source, and with `outputs` in its text outputs too. This
//! module only matches and formats; the tool in `mcp.rs` walks the
//! directory and reads each notebook, from its room when one is open.

use super::best_text;
use ganymede_nbformat::{Cell, Notebook, Output};
use serde_json::json;

/// Characters of context on each side of the first match.
const CONTEXT: usize = 80;

/// What to look in.
#[derive(Debug, Clone, Copy, Default)]
pub struct Scope<'a> {
    /// Search text outputs as well as sources.
    pub outputs: bool,
    /// Only cells of this type (`code`, `markdown`, `raw`).
    pub cell_type: Option<&'a str>,
}

/// A matching cell.
#[derive(Debug, PartialEq)]
pub struct Hit {
    pub index: usize,
    pub id: Option<String>,
    pub cell_type: &'static str,
    /// Where the first word matched: `source` or `output`.
    pub r#in: &'static str,
    pub excerpt: String,
}

/// The query's words, lowercased. Empty for a blank query.
pub fn words(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

/// The notebook's title: the first Markdown cell's first line, when it
/// is a heading.
pub fn title(nb: &Notebook) -> Option<String> {
    nb.cells.iter().find_map(|c| match c {
        Cell::Markdown(m) => {
            let first = m.source.as_str();
            let line = first.lines().find(|l| !l.trim().is_empty())?.trim();
            line.starts_with('#')
                .then(|| line.trim_start_matches('#').trim().to_string())
        }
        _ => None,
    })
}

/// The cells of `nb` that match every word in `words`.
pub fn search(nb: &Notebook, words: &[String], scope: Scope) -> Vec<Hit> {
    let mut hits = Vec::new();
    if words.is_empty() {
        return hits;
    }
    for (index, cell) in nb.cells.iter().enumerate() {
        let (kind, source, outs) = match cell {
            Cell::Code(c) => ("code", c.source.as_str(), Some(&c.outputs)),
            Cell::Markdown(c) => ("markdown", c.source.as_str(), None),
            Cell::Raw(c) => ("raw", c.source.as_str(), None),
            _ => continue,
        };
        if scope.cell_type.is_some_and(|t| t != kind) {
            continue;
        }
        let output = match (scope.outputs, outs) {
            (true, Some(outs)) => outputs_text(outs),
            _ => String::new(),
        };
        let (src, out) = (source.to_lowercase(), output.to_lowercase());
        if !words.iter().all(|w| src.contains(w) || out.contains(w)) {
            continue;
        }
        let first = &words[0];
        let (r#in, excerpt) = if src.contains(first.as_str()) {
            ("source", excerpt(source, first))
        } else {
            ("output", excerpt(&output, first))
        };
        hits.push(Hit {
            index,
            id: cell.id().map(str::to_string),
            cell_type: kind,
            r#in,
            excerpt,
        });
    }
    hits
}

/// The text of a cell's outputs: streams, the best text of each result
/// or display, and errors' name and value. Images contribute nothing.
fn outputs_text(outs: &[Output]) -> String {
    let mut s = String::new();
    for o in outs {
        let t = match o {
            Output::Stream(st) => st.text.as_str().to_string(),
            Output::DisplayData(d) => text_of(&d.data),
            Output::ExecuteResult(r) => text_of(&r.data),
            Output::Error(e) => format!("{}: {}", e.ename, e.evalue),
            Output::Unknown(_) => continue,
        };
        s.push_str(&ganymede_nbformat::strip_ansi(&t));
        s.push('\n');
    }
    s
}

fn text_of(data: &ganymede_nbformat::MimeBundle) -> String {
    let v = json!(data.0);
    match best_text(&v) {
        (Some(_), t) => t,
        (None, _) => String::new(),
    }
}

/// Up to [`CONTEXT`] characters around the first match of `word` (already
/// lowercase) in `text`, on one line, with `…` where it was cut.
fn excerpt(text: &str, word: &str) -> String {
    let lower = text.to_lowercase();
    // Lowercasing can change byte lengths; work in chars.
    let chars: Vec<char> = text.chars().collect();
    let at = match lower.find(word) {
        Some(b) => lower[..b].chars().count(),
        None => 0,
    };
    let len = word.chars().count();
    let start = at.saturating_sub(CONTEXT);
    let end = (at + len + CONTEXT).min(chars.len());
    let mut s: String = chars[start.min(end)..end].iter().collect();
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if start > 0 {
        s.insert(0, '…');
    }
    if end < chars.len() {
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nb(cells: serde_json::Value) -> Notebook {
        Notebook::from_value(json!({
            "nbformat": 4, "nbformat_minor": 5, "metadata": {},
            "cells": cells,
        }))
        .unwrap()
    }

    fn sample() -> Notebook {
        nb(json!([
            {"cell_type": "markdown", "id": "t", "metadata": {},
             "source": "# Fremont Bridge counts\n\nGoal: compare 2026 to 2019."},
            {"cell_type": "code", "id": "c", "metadata": {}, "execution_count": 1,
             "source": "weekdays().mean()",
             "outputs": [{"output_type": "execute_result", "execution_count": 1,
                          "metadata": {}, "data": {"text/plain": "\u{1b}[33m-0.19\u{1b}[39m"}}]},
            {"cell_type": "markdown", "id": "f", "metadata": {},
             "source": "Workday mornings fell 19% against 2019."},
        ]))
    }

    #[test]
    fn title_is_the_first_heading() {
        assert_eq!(title(&sample()).as_deref(), Some("Fremont Bridge counts"));
        let untitled = nb(json!([{"cell_type": "markdown", "id": "x",
                                  "metadata": {}, "source": "no heading"}]));
        assert_eq!(title(&untitled), None);
    }

    #[test]
    fn every_word_must_match_in_any_case() {
        let n = sample();
        let hits = search(&n, &words("2019 WORKDAY"), Scope::default());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id.as_deref(), Some("f"));
        assert_eq!(hits[0].r#in, "source");
        assert!(search(&n, &words("2019 bicycle"), Scope::default()).is_empty());
        assert!(search(&n, &words("  "), Scope::default()).is_empty());
    }

    #[test]
    fn outputs_only_when_asked_and_without_ansi() {
        let n = sample();
        assert!(search(&n, &words("-0.19"), Scope::default()).is_empty());
        let hits = search(
            &n,
            &words("-0.19"),
            Scope {
                outputs: true,
                ..Default::default()
            },
        );
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].index, hits[0].r#in), (1, "output"));
        assert_eq!(hits[0].excerpt, "-0.19");
    }

    #[test]
    fn cell_type_filters() {
        let n = sample();
        let only_code = Scope {
            cell_type: Some("code"),
            ..Default::default()
        };
        assert!(search(&n, &words("2019"), only_code).is_empty());
        assert_eq!(search(&n, &words("mean"), only_code).len(), 1);
    }

    #[test]
    fn excerpts_are_cut_around_the_match() {
        let long = format!("{} needle {}", "a ".repeat(100), "b ".repeat(100));
        let e = excerpt(&long, "needle");
        assert!(e.starts_with('…') && e.ends_with('…'), "{e}");
        assert!(e.contains("needle"));
        assert!(e.chars().count() < 2 * CONTEXT + 10);
        // Multi-byte text before the match.
        assert_eq!(excerpt("Ünïcödé needle", "needle"), "Ünïcödé needle");
    }
}
