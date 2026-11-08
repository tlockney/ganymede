## 2026-11-08, lab and ui open the redirect page (#539)

`ganymede lab` and `ganymede ui` passed the tokenized URL to `open`,
`xdg-open` or `explorer`, so the token showed in the process list while
the opener ran. `jupyter_server` opens `jpserver-<pid>-open.html` instead,
a page that redirects to the URL, and ganymede already writes that file
before it calls `on_listening`. The opener now gets that file's path, and
the URL only when the file is missing (the runtime dir couldn't be
written).

Review follow-ups: the server hands `lab`/`ui` the page it actually wrote
(`Startup::on_open_page`, `ServerInfoFiles::open_html`), so a stale file
from an earlier process with the same pid is never opened and the CLI no
longer rebuilds the server's file name. The page goes to the opener as a
`file://` URL, so it reaches the browser rather than the `.html` handler.
`--no-redirect-file` opens the URL instead, for snap and Flatpak browsers
that can't read the runtime dir; `docs/reference.md` says so.
