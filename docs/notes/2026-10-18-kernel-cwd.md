## 2026-10-18, kernel working directory stays under the root (#51)

`KernelManager::start` joined the client's `path` onto the root without
validating it, so `POST /api/kernels {"path": "../../etc"}` or a session
for `../../etc/x.ipynb` started a kernel outside the contents root, and a
path to a directory that did not exist failed the launch with a 500.

It now follows `jupyter_server`'s `cwd_for_path`:

- The path goes through the contents API-path validation
  ([`kernel_cwd`](../../crates/ganymede-contents/src/paths.rs)). Anything
  that does not validate (`..`, absolute, backslashes) gives the root.
- From there it walks up to the nearest existing directory, and falls back
  to the root. A directory that a symlink resolves outside the root also
  gives the root.
- `POST /api/kernels` takes `path` as a directory, as `jupyter_server`
  does; sessions pass their notebook's parent (`get_kernel_path`). Before,
  both took the parent, so a kernel started for directory `nb` ran in the
  root.

`ganymede-api` now depends on `ganymede-contents` for this, so there is one
definition of a valid API path.
