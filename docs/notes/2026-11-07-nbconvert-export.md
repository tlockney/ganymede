## 2026-11-07, script and Markdown export (#235)

JupyterLab's **File > Save and Export Notebook As** menu was empty:
`/api/nbconvert` listed no exporters, since nbconvert's are Python
templates. Two of its formats need no templates and no knowledge of the
kernel's language beyond `metadata.language_info`, so they are now
built in Rust:

- `ganymede-nbformat` has `to_script` and `to_markdown` (module
  `export`), which return the text, its extension and MIME type, and
  any files it refers to. The crate stays free of I/O and new
  dependencies; images come back as the base64 the notebook stores.
- `ganymede-server` lists `markdown` and `script` in `/api/nbconvert`
  and serves `GET /nbconvert/{format}/{path}` as `jupyter_server`'s
  `NbconvertFileHandler` does: `?download=true` for an attachment, a
  redirect to `/files` for a path that isn't a notebook, and a zip when
  the export has files. The zip uses the `zip` crate that `ganymede-cli`
  already depends on, with the same features, so `Cargo.lock` gains no
  new crate.

Choices:

- **Script**: code cells, and raw cells with no MIME type or the
  script's own (as nbconvert's `raw_mimetypes`), joined by two blank
  lines. Markdown cells are left out: commenting them needs the
  language's comment syntax, which the host doesn't know. nbconvert's
  generic script template leaves them out too; only per-language
  exporters (Python's) comment them. Extension and MIME type come from
  `language_info.file_extension` and `mimetype`, defaulting to `.txt`
  and `text/plain`.
- **Markdown**: nbconvert's Markdown template, except that text outputs
  and tracebacks are fenced blocks instead of indented ones, with a
  fence longer than any backtick run in them, and a carriage return in
  stream text starts its line over, so a progress bar shows its last
  state. Images (PNG, JPEG, SVG)
  and cell attachments are files under `<name>_files/`, and the
  response is then a zip of the `.md` and the files, as nbconvert's
  `respond_zip`. Data URIs were the alternative; GitHub and many
  viewers don't render them, and a zip matches what Jupyter users get.
- The script's MIME type comes from the notebook, so the response
  carries `/files`' sandbox `Content-Security-Policy`: a notebook that
  claims `text/html` can't run script as the server's origin.

Not done: `POST /nbconvert/{format}` (export of a posted notebook),
which JupyterLab doesn't call.
