## 2026-10-31, Server info file (#242)

`serve`, `lab` and `ui` now write `jpserver-<pid>.json` and `jpserver-<pid>-open.html` in the Jupyter runtime dir, with `jupyter_server`'s keys (`url`, `token`, `root_dir`, `pid`, `port`, ...), and remove them on exit. This is the first step of agent-native Ganymede (#239): `jupyter server list` lists a running Ganymede, and `ganymede mcp` (#245) and an nb-cli change can find a server without being handed its URL and token.

The files are written to a temporary file and renamed, so they are 0600 from the start and a reader never sees half a file; the runtime dir is created 0700 if missing. They are written before the URL is printed, so a script that waits for the URL can read them at once (the first version wrote them after, and the end-to-end test lost that race). `version` is `JUPYTER_SERVER_VERSION`, as `GET /api` reports; an extra `ganymede_version` key tells clients which server it is. `jupyter server list` ignores extra keys.

One difference: for a wildcard `--ip` (`0.0.0.0`, `::`), `jupyter_server` puts the machine's host name in `url`; Ganymede puts `localhost`, which avoids a `gethostname` dependency and is what a client on the same machine should use. Stale files from killed servers are left to `jupyter server list`, which deletes those whose pid is gone. The embedded server (`ganymede-ffi`) writes nothing.

The end-to-end test in `crates/ganymede-cli/tests/cli.rs` checks the file's contents and mode, removal on SIGTERM, and, when a reference `jupyter` is installed (CI), that `jupyter server list --json` returns exactly the file's contents.
