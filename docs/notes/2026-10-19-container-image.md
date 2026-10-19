## 2026-10-19, a container image with no Python (#203)

`container/Dockerfile` follows cdp-kernel's image, with one difference:
cdp-kernel starts from Jupyter's minimal-notebook image, which carries
Python and JupyterLab's server. This one is `debian:trixie-slim` with
Node 24 copied from the official image, cdp-kernel's `cdp-node`
kernelspec, and JupyterLab's frontend from `ganymede lab install`.
`container/smoke-test.sh` fails if `python3` or `python` is on `PATH`.

- The binary is the GitHub release named by `Cargo.toml`
  (`GANYMEDE_SOURCE=release`, the default), so release.yml's `image` job
  runs after the `release` job has published the archives. CI builds
  with `GANYMEDE_SOURCE=checkout` (Rust 1.95, the MSRV); a cold build
  takes about three minutes.
- Kernelspecs go in `/usr/local/share/jupyter` and JupyterLab in
  `/usr/local/share/ganymede` (`GANYMEDE_DATA_DIR`), so a volume over the
  home directory hides neither.
- `ganymede lab --ip 0.0.0.0` prints `http://0.0.0.0:8888/...`; the docs
  tell people to open it with `127.0.0.1`. `docker stop` works without
  `--init`: the server handles SIGTERM.
- With `--user` set to a uid that has no passwd entry, HOME is `/` and the
  server can't write its cookie secret or trust database; it still
  serves, with warnings. The docs say the mount must be writable by uid
  1000 instead.
- `ca-certificates` is in the image: `ganymede lab install` uses
  `rustls-platform-verifier`, which reads the system trust store.
