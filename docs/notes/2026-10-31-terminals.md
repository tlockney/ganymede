## 2026-10-31, Jupyter terminals API (#232)

Ganymede now serves `/api/terminals` and terminado's websocket at `/terminals/websocket/{name}`, so JupyterLab's Terminal launcher works and the Mac app can open a shell on its embedded server (tlockney/ganymede-ui#117). The new `ganymede-terminals` crate runs the user's login shell in a pseudo-terminal through `portable-pty` (WezTerm's; Unix PTYs and Windows ConPTY). The shell is `$SHELL -l` or `terminal_shell`, never a language, so the host still names none.

Choices that follow `jupyter_server` and terminado: unnamed terminals take the lowest free number from 1, a name in use is 409, `POST` answers 200 with the model, `DELETE` 204, and a relative `cwd` resolves against the root dir and falls back to it when missing. A client gets `["setup", {}]`, then the terminal's last 128 KiB of output, then live output; any number of clients share a terminal. `terminals_enabled = false` leaves the routes answering 404, as jupyter_server registers none then.

Lessons:

- The parent must drop its copy of the PTY slave after spawning, or the reader never sees EOF when the shell exits.
- `wait`, `kill` and `resize` each take their own lock. With one lock around the child, a delete could block behind the reader thread's `wait`.
- History and subscription are taken under the history lock, so a client attaching mid-output gets each chunk once.
- iOS and wasm get a stub: `portable-pty` is a target-specific dependency, `TerminalManager::supported()` is false there, and the server leaves terminals off. CI's iOS check now includes the crate.
