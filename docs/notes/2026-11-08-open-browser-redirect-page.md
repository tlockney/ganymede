## 2026-11-08, lab and ui open the redirect page (#539)

`ganymede lab` and `ganymede ui` passed the tokenized URL to `open`,
`xdg-open` or `explorer`, so the token showed in the process list while
the opener ran. `jupyter_server` opens `jpserver-<pid>-open.html` instead,
a page that redirects to the URL, and ganymede already writes that file
before it calls `on_listening`. The opener now gets that file's path, and
the URL only when the file is missing (the runtime dir couldn't be
written).
