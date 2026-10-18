## 2026-10-18, chunked uploads append (#52)

JupyterLab uploads files over 15 MB in 1 MB pieces, each a PUT with
`chunk: 1, 2, ...` and `-1` for the last. The contents layer ignored
`chunk` and saved every PUT as the whole file, so an upload kept only its
last megabyte, silently.

`SaveRequest` now carries `chunk`, and `FsContents::save` follows
jupyter_server's `LargeFileManager.save`: chunk 1 writes the file (atomic
replace, as a normal save), any other chunk appends its decoded `text` or
`base64` content. Only `type: file` can be chunked (400 otherwise), and
chunks skip the `last_modified` conflict check, as jupyter_server does.

The HTTP status follows from the existing 201-if-new rule: chunk 1 of a
new file is 201, later chunks 200, which is what jupyter_server answers.
