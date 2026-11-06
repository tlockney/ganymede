# ganymede-server

`jupyter_server`'s REST and websocket API in axum, over `ganymede-api` and
`ganymede-contents`. `ganymede serve` and `ganymede lab` run it; the
crate's own `ganymede-server` binary takes the same flags, for
development without the CLI.

```
cargo run -p ganymede-server -- --root-dir ~/notebooks --port 8888
# prints http://127.0.0.1:8888/?token=...
```

Flags and config keys are listed in
[CLI and configuration](../../docs/reference.md); `config.rs` has the
TOML keys, and an unknown key is an error.

The routes are listed in [The API by example](../../docs/api.md#every-route),
and authentication, the `Host` and `Origin` checks and the login cookie
in [Security](../../docs/security.md). `auth.rs` has the auth checks and
`lab.rs` JupyterLab's routes.

Integration tests in `tests/http.rs` run the router against the fake
kernel, including a real websocket round trip.
