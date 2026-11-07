//! Exporting a notebook as a script or as Markdown, the two nbconvert
//! formats that need no templates and no knowledge of the kernel's
//! language beyond `metadata.language_info`.
//!
//! - [`to_script`]: the code cells, joined. The file extension and MIME
//!   type come from `language_info` (`file_extension`, `mimetype`), with
//!   `.txt` and `text/plain` when it has none. Markdown cells are left
//!   out, since commenting them needs the language's comment syntax.
//! - [`to_markdown`]: markdown cells as they are, code cells as fenced
//!   blocks tagged with the language's name, text outputs as plain
//!   fenced blocks, and images (and cell attachments) as files under
//!   `<name>_files/`, which nbconvert's Markdown exporter also does. The
//!   caller packs the Markdown and the files together, as nbconvert's
//!   handler returns a zip when there are any.
//!
//! As nbconvert's templates, raw cells go in when their `raw_mimetype`
//! is empty or one the output format takes.

use crate::{Cell, MimeBundle, Notebook, Output};
use serde_json::{Map, Value};

/// An exported notebook.
#[derive(Debug, Clone, PartialEq)]
pub struct Export {
    /// The exported text.
    pub body: String,
    /// The file extension, with its leading dot: `.md`, or the script's.
    pub extension: String,
    /// The MIME type of `body`.
    pub mimetype: String,
    /// Files the body refers to by relative path, such as images.
    pub resources: Vec<Resource>,
}

/// A file an export refers to.
#[derive(Debug, Clone, PartialEq)]
pub struct Resource {
    /// Path relative to the exported file, `/`-separated.
    pub path: String,
    pub data: ResourceData,
}

/// A resource's content, as the notebook stores it.
#[derive(Debug, Clone, PartialEq)]
pub enum ResourceData {
    Text(String),
    /// Base64, as nbformat stores binary output data. May contain line
    /// breaks.
    Base64(String),
}

/// The export formats, by their nbconvert names, with the MIME type
/// `/api/nbconvert` reports for each. A script's actual type depends on
/// the notebook's language.
pub const FORMATS: &[(&str, &str)] = &[("markdown", "text/markdown"), ("script", "text/plain")];

/// The notebook's `metadata.language_info`.
fn language_info(nb: &Notebook) -> Option<&Map<String, Value>> {
    nb.metadata.get("language_info").and_then(Value::as_object)
}

fn language_info_str<'a>(nb: &'a Notebook, key: &str) -> Option<&'a str> {
    language_info(nb)
        .and_then(|li| li.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// A raw cell's target MIME type: `raw_mimetype`, or the older `format`.
fn raw_mimetype(metadata: &Map<String, Value>) -> String {
    metadata
        .get("raw_mimetype")
        .or_else(|| metadata.get("format"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// The notebook as a script in its kernel's language: its code cells, and
/// raw cells with no MIME type or the script's, separated by blank lines.
pub fn to_script(nb: &Notebook) -> Export {
    let extension = match language_info_str(nb, "file_extension") {
        Some(e) if e.starts_with('.') => e.to_owned(),
        Some(e) => format!(".{e}"),
        None => ".txt".to_owned(),
    };
    let mimetype = language_info_str(nb, "mimetype")
        .unwrap_or("text/plain")
        .to_owned();
    let mut parts: Vec<&str> = Vec::new();
    for cell in &nb.cells {
        let source = match cell {
            Cell::Code(c) => c.source.as_str(),
            Cell::Raw(c) => {
                let mime = raw_mimetype(&c.metadata);
                if !mime.is_empty() && mime != mimetype.to_ascii_lowercase() {
                    continue;
                }
                c.source.as_str()
            }
            Cell::Markdown(_) | Cell::Unknown(_) => continue,
        };
        let source = source.trim_end_matches(['\n', '\r']);
        if !source.trim().is_empty() {
            parts.push(source);
        }
    }
    let mut body = parts.join("\n\n\n");
    if !body.is_empty() {
        body.push('\n');
    }
    Export {
        body,
        extension,
        mimetype,
        resources: Vec::new(),
    }
}

/// Output MIME types in the order the Markdown export prefers them, as
/// nbconvert's `display_data_priority` (without PDF, which Markdown can't
/// show).
const PRIORITY: &[&str] = &[
    "text/html",
    "text/latex",
    "image/svg+xml",
    "image/png",
    "image/jpeg",
    "text/markdown",
    "text/plain",
];

/// The notebook as Markdown. `name` is the notebook's name without
/// `.ipynb`; images go under `<name>_files/`.
pub fn to_markdown(nb: &Notebook, name: &str) -> Export {
    let files_dir = format!("{name}_files");
    let language = language_info_str(nb, "name").unwrap_or("");
    let mut blocks: Vec<String> = Vec::new();
    let mut resources: Vec<Resource> = Vec::new();
    for (index, cell) in nb.cells.iter().enumerate() {
        match cell {
            Cell::Markdown(c) => {
                let mut source = c.source.as_str().trim_end().to_owned();
                for (file, bundle) in c.attachments.iter().flatten() {
                    let Some((path, data)) = attachment_file(&files_dir, index, file, bundle)
                    else {
                        continue;
                    };
                    source = source.replace(&format!("attachment:{file}"), &url_path(&path));
                    resources.push(Resource { path, data });
                }
                if !source.is_empty() {
                    blocks.push(source);
                }
            }
            Cell::Raw(c) => {
                let mime = raw_mimetype(&c.metadata);
                if matches!(mime.as_str(), "" | "text/markdown" | "text/html") {
                    let source = c.source.as_str().trim_end();
                    if !source.is_empty() {
                        blocks.push(source.to_owned());
                    }
                }
            }
            Cell::Code(c) => {
                let lang = c
                    .metadata
                    .get("magics_language")
                    .and_then(Value::as_str)
                    .unwrap_or(language);
                if !c.source.as_str().trim().is_empty() {
                    blocks.push(fenced(lang, c.source.as_str()));
                }
                for (o, output) in c.outputs.iter().enumerate() {
                    if let Some(block) = output_block(output, &files_dir, index, o, &mut resources)
                    {
                        blocks.push(block);
                    }
                }
            }
            Cell::Unknown(_) => {}
        }
    }
    let mut body = blocks.join("\n\n");
    if !body.is_empty() {
        body.push('\n');
    }
    Export {
        body,
        extension: ".md".to_owned(),
        mimetype: "text/markdown".to_owned(),
        resources,
    }
}

/// An attachment as a file: `<files_dir>/attachment_<cell>_<name>`, from
/// its first image or text entry.
fn attachment_file(
    files_dir: &str,
    cell: usize,
    file: &str,
    bundle: &MimeBundle,
) -> Option<(String, ResourceData)> {
    // The name is the notebook author's; keep it to one path segment.
    let safe: String = file
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let (mime, value) = bundle.iter().next()?;
    let text = value.as_str()?;
    let data = if mime.starts_with("image/") && mime != "image/svg+xml" {
        ResourceData::Base64(text.to_owned())
    } else {
        ResourceData::Text(text.to_owned())
    };
    Some((format!("{files_dir}/attachment_{cell}_{safe}"), data))
}

/// One output as a Markdown block, adding any image to `resources`.
fn output_block(
    output: &Output,
    files_dir: &str,
    cell: usize,
    index: usize,
    resources: &mut Vec<Resource>,
) -> Option<String> {
    let data = match output {
        Output::Stream(s) => {
            return text_block(&overwrite_returns(&crate::strip_ansi(s.text.as_str())))
        }
        Output::Error(e) => {
            let lines: Vec<String> = e.traceback.iter().map(|l| crate::strip_ansi(l)).collect();
            let text = if lines.is_empty() {
                format!("{}: {}", e.ename, e.evalue)
            } else {
                lines.join("\n")
            };
            return text_block(&text);
        }
        Output::DisplayData(d) => &d.data,
        Output::ExecuteResult(r) => &r.data,
        Output::Unknown(_) => return None,
    };
    let (mime, value) = PRIORITY
        .iter()
        .find_map(|m| data.get_str(m).map(|v| (*m, v)))?;
    let image = |ext: &str, data: ResourceData, resources: &mut Vec<Resource>| {
        let path = format!("{files_dir}/output_{cell}_{index}.{ext}");
        let block = format!("![{ext}]({})", url_path(&path));
        resources.push(Resource { path, data });
        Some(block)
    };
    match mime {
        "text/html" | "text/latex" | "text/markdown" => {
            let v = value.trim();
            (!v.is_empty()).then(|| v.to_owned())
        }
        "image/svg+xml" => image("svg", ResourceData::Text(value.to_owned()), resources),
        "image/png" => image("png", ResourceData::Base64(value.to_owned()), resources),
        "image/jpeg" => image("jpg", ResourceData::Base64(value.to_owned()), resources),
        _ => text_block(&crate::strip_ansi(value)),
    }
}

/// Stream text as a terminal shows it: on each line, a carriage return
/// starts the line over, as progress bars use it.
fn overwrite_returns(text: &str) -> String {
    text.split('\n')
        .map(|line| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            line.rsplit('\r').next().unwrap_or(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Text as a fenced block with no language, or nothing for empty text.
fn text_block(text: &str) -> Option<String> {
    let text = text.trim_end();
    (!text.trim().is_empty()).then(|| fenced("", text))
}

/// `text` in a fenced code block, with a fence longer than any run of
/// backticks in it, so the text can't close the block.
fn fenced(lang: &str, text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat((longest + 1).max(3));
    let text = text.trim_end_matches(['\n', '\r']);
    // A fence's info string can't contain backticks.
    let lang: String = lang
        .chars()
        .filter(|c| *c != '`' && !c.is_whitespace())
        .collect();
    format!("{fence}{lang}\n{text}\n{fence}")
}

/// A relative path as a Markdown link target: each byte outside the URL
/// unreserved set (and `/`) percent-encoded, as nbconvert's `path2url`.
fn url_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"/_.-~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn nb(value: Value) -> Notebook {
        serde_json::from_value(value).unwrap()
    }

    fn sample() -> Notebook {
        nb(json!({
            "nbformat": 4, "nbformat_minor": 5,
            "metadata": {"language_info": {
                "name": "typescript", "file_extension": ".ts", "mimetype": "text/typescript"
            }},
            "cells": [
                {"cell_type": "markdown", "id": "a", "metadata": {}, "source": "# Title\n\nSome text."},
                {"cell_type": "code", "id": "b", "metadata": {}, "execution_count": 1,
                 "source": "const x = 1;\nconsole.log(x);\n",
                 "outputs": [
                    {"output_type": "stream", "name": "stdout", "text": "1\r\n0%\r50%\r100%\n"},
                    {"output_type": "execute_result", "execution_count": 1, "metadata": {},
                     "data": {"text/plain": "1", "image/png": "iVBORw0KGgo=\n"}}
                 ]},
                {"cell_type": "code", "id": "c", "metadata": {}, "execution_count": null,
                 "source": "   ", "outputs": []},
                {"cell_type": "raw", "id": "d", "metadata": {}, "source": "raw text"},
                {"cell_type": "raw", "id": "e", "metadata": {"raw_mimetype": "text/x-rst"}, "source": "rst"},
                {"cell_type": "code", "id": "f", "metadata": {}, "execution_count": 2,
                 "source": "throw new Error('no')",
                 "outputs": [{"output_type": "error", "ename": "Error", "evalue": "no",
                              "traceback": ["\u{1b}[31mError\u{1b}[0m: no", "    at <anonymous>"]}]}
            ]
        }))
    }

    #[test]
    fn script_joins_code_cells_with_language_info() {
        let e = to_script(&sample());
        assert_eq!(e.extension, ".ts");
        assert_eq!(e.mimetype, "text/typescript");
        assert!(e.resources.is_empty());
        assert_eq!(
            e.body,
            "const x = 1;\nconsole.log(x);\n\n\nraw text\n\n\nthrow new Error('no')\n"
        );
    }

    #[test]
    fn script_without_language_info_is_text() {
        let mut n = sample();
        n.metadata.clear();
        let e = to_script(&n);
        assert_eq!(
            (e.extension.as_str(), e.mimetype.as_str()),
            (".txt", "text/plain")
        );

        n.metadata = json!({"language_info": {"file_extension": "rs"}})
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(to_script(&n).extension, ".rs");
        assert_eq!(to_script(&Notebook::new()).body, "");
    }

    #[test]
    fn markdown_fences_code_and_extracts_images() {
        let e = to_markdown(&sample(), "my nb");
        assert_eq!(e.extension, ".md");
        assert_eq!(e.mimetype, "text/markdown");
        assert_eq!(
            e.body,
            "# Title\n\nSome text.\n\n\
             ```typescript\nconst x = 1;\nconsole.log(x);\n```\n\n\
             ```\n1\n100%\n```\n\n\
             ![png](my%20nb_files/output_1_1.png)\n\n\
             raw text\n\n\
             ```typescript\nthrow new Error('no')\n```\n\n\
             ```\nError: no\n    at <anonymous>\n```\n"
        );
        assert_eq!(
            e.resources,
            vec![Resource {
                path: "my nb_files/output_1_1.png".into(),
                data: ResourceData::Base64("iVBORw0KGgo=\n".into()),
            }]
        );
    }

    #[test]
    fn markdown_prefers_html_and_falls_back_to_text() {
        let n = nb(json!({
            "nbformat": 4, "nbformat_minor": 5, "metadata": {},
            "cells": [{"cell_type": "code", "id": "a", "metadata": {"magics_language": "bash"},
                "execution_count": 1, "source": "ls",
                "outputs": [
                    {"output_type": "display_data", "metadata": {},
                     "data": {"text/plain": "<table>", "text/html": "<table></table>\n"}},
                    {"output_type": "display_data", "metadata": {},
                     "data": {"application/vnd.custom+json": {}, "text/plain": "has ``` in it"}}
                ]}]
        }));
        let e = to_markdown(&n, "n");
        assert_eq!(
            e.body,
            "```bash\nls\n```\n\n<table></table>\n\n````\nhas ``` in it\n````\n"
        );
        assert!(e.resources.is_empty());
    }

    #[test]
    fn markdown_attachments_become_files() {
        let n = nb(json!({
            "nbformat": 4, "nbformat_minor": 5, "metadata": {},
            "cells": [{"cell_type": "markdown", "id": "a", "metadata": {},
                "source": "![pic](attachment:a b.png)",
                "attachments": {"a b.png": {"image/png": "AAAA"}}}]
        }));
        let e = to_markdown(&n, "n");
        assert_eq!(e.body, "![pic](n_files/attachment_0_a%20b.png)\n");
        assert_eq!(e.resources[0].path, "n_files/attachment_0_a b.png");
    }
}
