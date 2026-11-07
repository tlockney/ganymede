## 2026-11-07, reap a deleted terminal's shell (#486)

`TerminalManager::delete` drops the terminal from the map and kills its
shell, and the reader thread waits for the shell once the pseudo-terminal
closes. But the thread held only a `Weak<Terminal>`, and the child handle
lived in the terminal: with no client still connected, the terminal was
gone by the time the read ended, the thread returned early, and the
killed shell stayed a zombie until the server exited.

The child handle is now shared (`pty::Waiter`), and the reader thread
waits through its own copy before it looks at the terminal. The test
checks with `kill -0`, which still succeeds on a zombie, so it fails
only once the shell is reaped.
