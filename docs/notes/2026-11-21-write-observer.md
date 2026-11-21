## 2026-11-21, Write observer for the app (#916)

The Mac and iPad apps keep answers per notebook file (ganymede-ui#584) and
must forget them when the file is replaced outside Ganymede, but not when
the server writes it. The app can stamp a file itself, but most writes on
its server come from the server with no window involved: a room's
autosave, a run saving its outputs, MCP. So the server now tells the app.

`ganymede_server::write_observer::WriteObserver` holds an optional
function, kept by the app and passed in `Startup::write_observer` as the
machines are, and settable while the server runs. `Stamping::rewrite` (every
save, a room's included, and a checkpoint restore) and the copy over an
existing file in `Stamping::copy` take the file's stamp inside the path's
turn before the write and again after it and its restamp, and call the
function once with the path and both stamps, all inside the
detached block, so a write whose caller went away is still reported. No
stamp is read while no function is set. `ganymede_server_set_write_observer`
in ganymede-ffi is the C side, with the callback's context passed back as
a raw pointer the app guarantees usable from any thread. Not a REST
endpoint: that API is a Jupyter compatibility target.

Reported: any successful save of a regular file (a new one's included)
and a copy to a named destination path, with the canonical absolute path
(links resolved, so a root under `/` and the app's `/private/...` agree).
Not reported: a failed write, a copy into a directory, a new untitled file,
a rename, a delete, a directory save, and any write when the contents
aren't a local directory. Renames by any client are not reported, so an app
that keys answers by path loses them on a rename it did not make itself,
which fails safe. The function is called with the path's turn held (the
next write or a rename of that path waits; keep it quick), not the
observer's own lock, so it may call `set`. A write that began while no function was set is not
reported even if one is set before it ends, as its "before" was never
taken. A path with a NUL byte is not passed to the callback. A new
`ganymede_contents::api_file_path` gives the on-disk path the stamps use.
