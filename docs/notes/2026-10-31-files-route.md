## 2026-10-31, raw files at /files (#233)

`GET /files/{path}` now serves a file's bytes. Without it, JupyterLab's
Download, Open in New Browser Tab, and relative images in Markdown
cells were broken, because the contents API's `getDownloadUrl` points
there.

- The handler copies `jupyter_server`'s `FilesHandler`. It calls
  `cm.get(path, type="file")` and decodes base64, so root confinement and
  the hidden-file policy come from the contents manager and work for any
  `Contents` implementation. The cost is that a large file is read whole
  and base64-encoded first. `jupyter_server` does the same.
- A directory gets 400, not 404, because that's what
  `get(type="file")` answers in both servers.
- `Content-Type` comes from the name. `.ipynb` is
  `application/x-ipynb+json`, and `text/plain` gets
  `; charset=UTF-8`. Python's `mimetypes` encodings map a `.gz`
  suffix to `application/gzip` and other compression suffixes to
  `application/octet-stream`. When the type can't be guessed, it
  depends on the format the file was read in: text is `text/plain`,
  base64 is `application/octet-stream`. Guessing uses `mime_guess`,
  which was already in the lockfile through `ganymede-contents`.
- `?download` sets `Content-Disposition` for any non-empty value
  (Tornado's `get_argument` truthiness), including `0`. The filename
  goes in RFC 5987 form, as `jupyter_server` writes it.
