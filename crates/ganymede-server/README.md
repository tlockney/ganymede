# ganymede-server

`jupyter_server`'s REST and websocket API in axum, over `ganymede-api` and
`ganymede-contents`.

```
cargo run -p ganymede-server -- --root-dir ~/notebooks --port 8888
# prints http://127.0.0.1:8888/?token=...
```

Flags: `--config file.toml`, `--ip`, `--port`, `--token` (empty disables
auth), `--root-dir`, `--static-dir` (frontend assets served at `/`, or
JupyterLab at `/lab` when it is JupyterLab's `share/jupyter/lab/static`),
`--labextensions-path` (repeatable), `--config-dir` (frontend state;
default `JUPYTER_CONFIG_DIR` or `~/.jupyter`), `--kernelspec-root`
(repeatable; a data dir containing `kernels/`), `--default-kernel`,
`--cull-idle-secs`.
`config.rs` lists the TOML keys, including `checkpoint_dir` for synced
roots, `base_url` for a reverse proxy, and `allow_origins` for websockets.

Routes: `/api` (public), `/api/status`, `/api/me`, `/api/config/{section}`,
`/api/nbconvert`, `/api/events/subscribe`, `/login`, `/logout`, `/api/kernelspecs[/{name}]`,
`/kernelspecs/{name}/{file}`, `/api/kernels` with `interrupt`, `restart`,
and the `channels` websocket, `/api/sessions`, `/api/contents/{path}` with
checkpoints. In JupyterLab mode (`lab.rs`): the page, `/static/lab`,
`/lab/extensions/{name}`, and `/lab/api/` `settings`, `workspaces`,
`translations`, `themes`, `listings`, `plugins`, `licenses`. The websocket speaks
`v1.kernel.websocket.jupyter.org` when offered and legacy JSON otherwise.

Auth (`auth.rs`) is `Authorization: token <t>`, `?token=<t>`, or the
login cookie, as in `jupyter_server`. A token-authenticated request
without the cookie gets one; a page URL with `?token=` also redirects to
itself without the token. `/login` takes the token in a form and `/logout`
clears the cookie. Pages send a browser without a login to `/login`; the
API answers 403. Cookie-authenticated requests are refused from other
origins, and those other than GET must echo the `_xsrf` cookie in
`X-XSRFToken` (or `_xsrf`). The login cookie is HttpOnly, SameSite=Lax, scoped to
`base_url`, and signed with a key kept in `jupyter_cookie_secret` in the
Jupyter runtime dir (`cookie_secret_file` in the config), so logins
survive a restart. Websocket upgrades also check `Origin` against the
request host and `allow_origins`.

Verified with `@jupyterlab/services` 7.5 in Node against ipykernel: specs,
start, execute, complete, interrupt, restart, contents, sessions, shutdown.
JSON bodies are parsed regardless of `Content-Type`, because that client
omits the header on some requests.

Integration tests in `tests/http.rs` run the router against the fake
kernel, including a real websocket round trip.
