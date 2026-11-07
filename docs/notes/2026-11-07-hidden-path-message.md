## 2026-11-07, a hidden path's 404 says it is hidden (ganymede-ui#337)

The Mac app roots its server at `/`, so a notebook under a dot-folder
(`~/x/.work/nb/a.ipynb`) is a hidden path. The contents API answers a
GET for a hidden path with 404, as `jupyter_server` does, but the
message was "no such file or directory", and the app showed that for a
file the user had just opened from Finder.

`resolve_for_read` now returns a new `Error::Hidden` when the hidden
path exists: still 404, with a message that says the path is hidden and
why Ganymede won't open it. A hidden path that doesn't exist is still
`NotFound`. Listings still leave hidden entries out, and writes to them
are still 400.

Opening such a notebook would need `allow_hidden` for that path, which
would also need saves to allow it; the issue accepts the message
instead. The apps pick this up when ganymede-ui's `GANYMEDE_REF` moves
past the release that carries it.
