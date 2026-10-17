# ganymede-server

`jupyter_server`'s REST and websocket API in axum, over `ganymede-api` and
`ganymede-contents`.

```
cargo run -p ganymede-server -- --root-dir ~/notebooks --port 8888
# prints http://127.0.0.1:8888/?token=...
```

Flags: `--config file.toml`, `--ip`, `--port`, `--token` (empty disables
auth), `--root-dir`, `--static-dir` (frontend assets served at `/`),
`--kernelspec-root` (repeatable), `--default-kernel`, `--cull-idle-secs`.
`config.rs` lists the TOML keys, including `checkpoint_dir` for synced
roots, `base_url` for a reverse proxy, and `allow_origins` for websockets.

Routes: `/api/status`, `/api/kernelspecs[/{name}]`, `/api/kernels` with
`interrupt`, `restart`, and the `channels` websocket, `/api/sessions`,
`/api/contents/{path}` with checkpoints. The websocket speaks
`v1.kernel.websocket.jupyter.org` when offered and legacy JSON otherwise.

Auth is `Authorization: token <t>` or `?token=<t>`. Websocket upgrades
also check `Origin` against the request host and `allow_origins`. There is
no cookie login yet, so a browser frontend needs the token in every
request; VS Code and `@jupyterlab/services` do that already.

Verified with `@jupyterlab/services` 7.5 in Node against ipykernel: specs,
start, execute, complete, interrupt, restart, contents, sessions, shutdown.
JSON bodies are parsed regardless of `Content-Type`, because that client
omits the header on some requests.

Integration tests in `tests/http.rs` run the router against the fake
kernel, including a real websocket round trip.
