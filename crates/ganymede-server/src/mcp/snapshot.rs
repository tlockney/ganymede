//! `cell_snapshot` (#611): a cell as Markdown an agent can put in front of
//! its user. The code goes in a fenced block, images come out as files,
//! HTML tables become Markdown tables and other output is plain text.
//!
//! This module only shapes the snapshot. The tool in `mcp.rs` writes the
//! images and swaps in their paths.

use super::{best_text, strip_html, svg_source};
use crate::svg;
use base64::Engine as _;
use ganymede_nbformat::{Cell, Output};
use serde_json::Value;

/// Text output longer than this is cut.
const TEXT_LIMIT: usize = 4000;

/// A piece of a snapshot: Markdown, or an image for the tool to save.
#[derive(Debug, PartialEq)]
pub enum Part {
    Markdown(String),
    Image(Image),
}

#[derive(Debug, PartialEq)]
pub struct Image {
    /// For a Markdown cell's attachment: its name, whose
    /// `attachment:<name>` links in the cell's Markdown point at the file.
    pub attachment: Option<String>,
    pub mime: &'static str,
    /// Base64, as in the notebook.
    pub data: String,
    /// The file extension for `mime`.
    pub ext: &'static str,
}

/// The fence language for a notebook's code: its `language_info.name`,
/// else its kernelspec's `language`.
pub fn language(metadata: &serde_json::Map<String, Value>) -> String {
    metadata
        .get("language_info")
        .and_then(|l| l.get("name"))
        .or_else(|| metadata.get("kernelspec").and_then(|k| k.get("language")))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The snapshot of `cell` (at `index`): a heading, its code when `code`,
/// then each output.
pub fn cell(index: usize, cell: &Cell, lang: &str, code: bool, max_rows: usize) -> Vec<Part> {
    let id = safe_name(cell.id().unwrap_or("?"));
    let mut parts = vec![Part::Markdown(format!("### Cell {index} (`{id}`)\n"))];
    match cell {
        Cell::Code(c) => {
            if code {
                parts.push(Part::Markdown(fence(lang, c.source.as_str())));
            }
            for o in &c.outputs {
                parts.extend(output(o, max_rows));
            }
            if c.outputs.is_empty() {
                parts.push(Part::Markdown("(no output)\n".into()));
            }
        }
        Cell::Markdown(c) => {
            parts.push(Part::Markdown(format!("{}\n", c.source.as_str())));
            for (name, bundle) in c.attachments.iter().flatten() {
                let data = serde_json::to_value(bundle).unwrap_or_default();
                if let Some(mut image) = image(&data) {
                    image.attachment = Some(name.clone());
                    parts.push(Part::Image(image));
                }
            }
        }
        Cell::Raw(c) => parts.push(Part::Markdown(fence("", c.source.as_str()))),
        _ => {}
    }
    parts
}

fn output(o: &Output, max_rows: usize) -> Vec<Part> {
    let v = serde_json::to_value(o).unwrap_or_default();
    match v["output_type"].as_str().unwrap_or_default() {
        "stream" => {
            let text = v["text"].as_str().unwrap_or_default();
            let text = ganymede_nbformat::overwrite_returns(&clean(text));
            vec![Part::Markdown(fence("text", &cut(&text)))]
        }
        "error" => {
            let e = format!(
                "{}: {}",
                v["ename"].as_str().unwrap_or_default(),
                v["evalue"].as_str().unwrap_or_default()
            );
            vec![Part::Markdown(fence("text", &cut(&clean(&e))))]
        }
        _ => {
            let data = &v["data"];
            if let Some(image) = image(data) {
                return vec![Part::Image(image)];
            }
            if let Some(html) = data.get("text/html").and_then(Value::as_str) {
                if let Some(table) = html_table(html, max_rows) {
                    return vec![Part::Markdown(table)];
                }
            }
            match best_text(data) {
                // Cut Markdown could end inside a fence and turn the rest
                // of the snapshot into code, so a long one goes as text.
                (Some("text/markdown"), md) if md.chars().count() > TEXT_LIMIT => {
                    vec![Part::Markdown(fence("text", &cut(md.trim_end())))]
                }
                (Some("text/markdown"), md) => vec![Part::Markdown(format!("{}\n", md.trim_end()))],
                (_, text) => vec![Part::Markdown(fence("text", &cut(&clean(&text))))],
            }
        }
    }
}

/// An output's image: a raster image as it is, else its SVG drawn as PNG.
pub fn image(data: &Value) -> Option<Image> {
    let b64 = &base64::engine::general_purpose::STANDARD;
    for (mime, ext) in [
        ("image/png", "png"),
        ("image/jpeg", "jpg"),
        ("image/gif", "gif"),
        ("image/webp", "webp"),
    ] {
        if let Some(raw) = data[mime].as_str() {
            let clean: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
            if b64.decode(&clean).is_ok() {
                return Some(Image {
                    attachment: None,
                    mime,
                    data: clean,
                    ext,
                });
            }
        }
    }
    let png = svg_source(&data["image/svg+xml"]).and_then(|s| svg::to_png(&s))?;
    Some(Image {
        attachment: None,
        mime: "image/png",
        data: b64.encode(png),
        ext: "png",
    })
}

/// The file name for a snapshot image: the notebook's stem, the cell id
/// (only `[A-Za-z0-9_-]` kept, as nbformat allows, since ids aren't
/// validated), its number, and a short hash of the image, so a later
/// snapshot of a re-run cell doesn't overwrite a file an earlier reply
/// links to.
pub fn file_name(stem: &str, id: &str, n: usize, image: &Image) -> String {
    use sha2::Digest;
    let hash = hex::encode(sha2::Sha256::digest(image.data.as_bytes()));
    format!(
        "{}-{}-{n}-{}.{}",
        safe_name(stem),
        safe_name(id),
        &hash[..8],
        image.ext
    )
}

/// `s` with only `[A-Za-z0-9_-]` kept, the rest `_`: for file names and
/// Markdown, from a notebook name or a cell id that nothing validates.
pub fn safe_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// A path as a Markdown link destination: in angle brackets, so spaces
/// and parentheses in it don't end the link, with `<` and `>` encoded.
pub fn link_destination(path: &str) -> String {
    // `%` first, so the escapes added after it stay as they are. A `#` or
    // `?` would start a fragment or query.
    let encoded = path
        .replace('%', "%25")
        .replace('<', "%3C")
        .replace('>', "%3E")
        .replace('#', "%23")
        .replace('?', "%3F");
    format!("<{encoded}>")
}

fn clean(s: &str) -> String {
    ganymede_nbformat::strip_ansi(s)
}

fn cut(s: &str) -> String {
    let n = s.chars().count();
    if n <= TEXT_LIMIT {
        return s.to_string();
    }
    let head: String = s.chars().take(TEXT_LIMIT).collect();
    format!("{head}\n[{} more characters]", n - TEXT_LIMIT)
}

/// `body` in a fence longer than any backtick run inside it.
fn fence(lang: &str, body: &str) -> String {
    format!("{}\n", ganymede_nbformat::fenced(lang, body))
}

/// The first `<table>` in `html` as a Markdown table: the first row is the
/// header, and at most `max_rows` rows follow it. `None` without a table.
pub fn html_table(html: &str, max_rows: usize) -> Option<String> {
    // ASCII lowercasing keeps byte offsets, so `lower`'s match `html`'s.
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<table")?;
    let end = lower[start..]
        .find("</table>")
        .map_or(html.len(), |e| start + e);
    let (html, lower) = (&html[start..end], &lower[start..end]);
    // Rows in `<thead>` after its first are header too: Polars' dtypes,
    // pandas' index name. They're left out rather than read as data.
    let thead_end = lower.find("</thead>");
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut pos = 0;
    while let Some(r) = lower[pos..].find("<tr") {
        let row_start = pos + r;
        let row_end = lower[row_start + 3..]
            .find("<tr")
            .map_or(lower.len(), |e| row_start + 3 + e);
        let extra_header = !rows.is_empty() && thead_end.is_some_and(|e| row_start < e);
        if !extra_header {
            rows.push(cells(&html[row_start..row_end], &lower[row_start..row_end]));
        }
        pos = row_end;
    }
    rows.retain(|r| !r.is_empty());
    let width = rows.iter().map(Vec::len).max()?;
    let line = |r: &[String]| {
        let mut cols: Vec<&str> = r.iter().map(String::as_str).collect();
        cols.resize(width, "");
        format!("| {} |\n", cols.join(" | "))
    };
    let mut out = line(&rows[0]);
    out.push_str(&format!("|{}\n", " --- |".repeat(width)));
    let body = &rows[1..];
    for r in body.iter().take(max_rows) {
        out.push_str(&line(r));
    }
    if body.len() > max_rows {
        out.push_str(&format!("\n({} more rows)\n", body.len() - max_rows));
    }
    Some(out)
}

/// The `<th>` and `<td>` texts in one row.
fn cells(html: &str, lower: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut pos = 0;
    loop {
        let th = lower[pos..].find("<th");
        let td = lower[pos..].find("<td");
        let Some(open) = th.into_iter().chain(td).min() else {
            break;
        };
        let open = pos + open;
        // `<thead>` is not a cell.
        if lower[open..].starts_with("<thead") {
            pos = open + 6;
            continue;
        }
        let Some(gt) = lower[open..].find('>') else {
            break;
        };
        let body_start = open + gt + 1;
        let close = ["</th", "</td", "<th", "<td"]
            .iter()
            .filter_map(|t| lower[body_start..].find(t))
            .min()
            .map_or(lower.len(), |e| body_start + e);
        let text = strip_html(&html[body_start..close]);
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        out.push(text.replace('|', "\\|"));
        pos = close;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn code_cell(source: &str, outputs: Value) -> Cell {
        serde_json::from_value(json!({
            "cell_type": "code", "id": "c1", "metadata": {}, "execution_count": 1,
            "source": source, "outputs": outputs
        }))
        .unwrap()
    }

    fn markdown(parts: &[Part]) -> String {
        parts
            .iter()
            .map(|p| match p {
                Part::Markdown(s) => s.clone(),
                Part::Image(i) => format!("[image {}]\n", i.ext),
            })
            .collect()
    }

    #[test]
    fn code_text_and_images() {
        let png = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG");
        let c = code_cell(
            "const x = 1;\nx",
            json!([
                {"output_type": "stream", "name": "stdout", "text": "\u{1b}[32mhi\u{1b}[39m\n"},
                {"output_type": "display_data", "metadata": {},
                 "data": {"image/png": png, "text/plain": "<Figure>"}},
                {"output_type": "execute_result", "execution_count": 1, "metadata": {},
                 "data": {"text/markdown": "| a |\n| --- |\n| 1 |"}},
                {"output_type": "error", "ename": "TypeError", "evalue": "boom", "traceback": []},
            ]),
        );
        let parts = cell(3, &c, "typescript", true, 20);
        assert_eq!(
            markdown(&parts),
            "### Cell 3 (`c1`)\n```typescript\nconst x = 1;\nx\n```\n```text\nhi\n```\n\
             [image png]\n| a |\n| --- |\n| 1 |\n```text\nTypeError: boom\n```\n"
        );
        let without_code = markdown(&cell(3, &c, "typescript", false, 20));
        assert!(!without_code.contains("const x"), "{without_code}");
    }

    #[test]
    fn file_names_and_links_survive_odd_names() {
        let image = |data: &str| Image {
            attachment: None,
            mime: "image/png",
            data: data.into(),
            ext: "png",
        };
        let a = file_name("Q#3 results?", "a/b c", 1, &image("AAAA"));
        assert!(
            a.starts_with("Q_3_results_-a_b_c-1-") && a.ends_with(".png"),
            "{a}"
        );
        assert_ne!(a, file_name("Q#3 results?", "a/b c", 1, &image("BBBB")));
        assert_eq!(
            link_destination("/x/My (1) #2? 50% <a>.png"),
            "</x/My (1) %232%3F 50%25 %3Ca%3E.png>"
        );
    }

    #[test]
    fn odd_ids_stay_in_their_code_span() {
        let mut c = code_cell("1", json!([]));
        if let Cell::Code(code) = &mut c {
            code.id = Some("a`b]c".into());
        }
        assert!(markdown(&cell(0, &c, "", false, 20)).starts_with("### Cell 0 (`a_b_c`)"));
    }

    #[test]
    fn progress_bars_show_their_last_frame() {
        let c = code_cell(
            "",
            json!([{"output_type": "stream", "name": "stdout", "text": "10%\r50%\r100%\ndone\n"}]),
        );
        let md = markdown(&cell(0, &c, "", false, 20));
        assert!(md.contains("```text\n100%\ndone\n```"), "{md}");
    }

    #[test]
    fn markdown_attachments_become_images() {
        let png = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG");
        let c: Cell = serde_json::from_value(json!({
            "cell_type": "markdown", "id": "m", "metadata": {},
            "source": "See ![plot](attachment:p.png)",
            "attachments": {"p.png": {"image/png": png}}
        }))
        .unwrap();
        let parts = cell(0, &c, "", false, 20);
        match parts.last() {
            Some(Part::Image(i)) => assert_eq!(i.attachment.as_deref(), Some("p.png")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn long_markdown_with_a_fence_is_fenced_as_text() {
        let md = format!("```\n{}", "x".repeat(TEXT_LIMIT + 10));
        let c = code_cell(
            "",
            json!([{"output_type": "display_data", "metadata": {}, "data": {"text/markdown": md}}]),
        );
        let out = markdown(&cell(0, &c, "", false, 20));
        // The text's own ``` is inside a longer fence, which closes.
        assert!(out.contains("````text\n```\n"), "{out}");
        assert!(out.trim_end().ends_with("````"), "{out}");
    }

    #[test]
    fn long_errors_and_markdown_are_cut() {
        let long = "x".repeat(TEXT_LIMIT + 10);
        let c = code_cell(
            "",
            json!([
                {"output_type": "error", "ename": "E", "evalue": long, "traceback": []},
                {"output_type": "display_data", "metadata": {}, "data": {"text/markdown": long}},
            ]),
        );
        let md = markdown(&cell(0, &c, "", false, 20));
        assert_eq!(md.matches("more characters]").count(), 2, "{md}");
    }

    #[test]
    fn fences_outlast_backticks_in_the_code() {
        assert_eq!(fence("md", "a ```` b"), "`````md\na ```` b\n`````\n");
    }

    #[test]
    fn html_tables_become_markdown() {
        let html = r#"<div><style>.x{}</style><TABLE border="1"><thead><tr><th></th><th>name</th><th>n</th></tr></thead>
            <tbody><tr><th>0</th><td>a | b</td><td>1</td></tr>
            <tr><th>1</th><td><b>c</b></td><td>2</td></tr>
            <tr><th>2</th><td>d</td><td>3</td></tr></tbody></TABLE></div>"#;
        assert_eq!(
            html_table(html, 2).unwrap(),
            "|  | name | n |\n| --- | --- | --- |\n| 0 | a \\| b | 1 |\n| 1 | c | 2 |\n\n(1 more rows)\n"
        );
        assert_eq!(html_table("<p>no table</p>", 5), None);
        // Polars: a dtype row in <thead> after the names.
        let polars =
            "<table><thead><tr><th>a</th><th>b</th></tr><tr><td>i64</td><td>str</td></tr></thead>\
            <tbody><tr><td>1</td><td>x</td></tr></tbody></table>";
        assert_eq!(
            html_table(polars, 5).unwrap(),
            "| a | b |\n| --- | --- |\n| 1 | x |\n"
        );
        let c = code_cell(
            "df",
            json!([{"output_type": "execute_result", "execution_count": 1, "metadata": {},
                    "data": {"text/html": html, "text/plain": "shape: (3, 2)"}}]),
        );
        assert!(markdown(&cell(0, &c, "", false, 20)).contains("| 2 | d | 3 |"));
    }

    #[test]
    fn language_from_the_notebook() {
        let m = |v: Value| v.as_object().unwrap().clone();
        assert_eq!(
            language(&m(json!({"kernelspec": {"language": "typescript"}}))),
            "typescript"
        );
        assert_eq!(
            language(&m(json!({"language_info": {"name": "python"},
                "kernelspec": {"language": "python3"}}))),
            "python"
        );
        assert_eq!(language(&m(json!({}))), "");
    }
}
