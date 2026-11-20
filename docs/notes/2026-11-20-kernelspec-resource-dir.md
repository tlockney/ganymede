## 2026-11-20, kernelspec resource_dir in the API (#890)

`GET /api/kernelspecs` and `GET /api/kernelspecs/{name}` now carry each
kernelspec's directory as `resource_dir`, so a client can open its
`kernel.json` or logos. It was only in `ganymede kernelspec list --json`.

- `KernelspecModel` gets `resource_dir: Option<String>`, filled in
  `spec_model` from `Kernelspec::resource_dir` (the one place both routes
  build the model). It is `#[serde(default, skip_serializing_if)]`: absent
  on the wire when the path is empty (gateway specs, which
  `Gateway::refresh` builds from the remote's `spec` alone, and in-app
  kernels from `RegisteredKernels`), and a model without it still parses.
- JupyterLab's `@jupyterlab/services` 7.6.6 (`kernelspec/validate.js`)
  checks only `name`, `resources` and `spec` fields and copies named
  properties, so the extra key is ignored.
- It exposes a server path to the callers the API lets in (the token, its
  login cookie, an allowed tailnet login, anyone when auth is off), who can
  already start kernels.
- `validation/jupyterlab.mjs` has no kernelspec assertion to extend, so
  only the read of `validate.js` above covers JupyterLab.
- `crates/ganymede-cli/tests/server.rs` (the jupyter_server comparison)
  records each kernelspec entry's keys without `resource_dir`, and checks
  Ganymede's `resource_dir` is the spec's directory (it holds `kernel.json`).
- Tests: `kernelspecs_kernels_and_sessions_rest` (list and get),
  `embedded.rs` and `gateway.rs` (key absent).
- `docs/api.md` documents it, with an Unreleased note.
