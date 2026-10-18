## 2026-10-18, `--existing` finds kernels in the runtime dir (#56)

`ganymede kernel` writes `kernel-<id>.json` into `jupyter_runtime_dir()`,
but `run --existing kernel-<id>.json` read the name relative to the
current directory, so the README's own example failed anywhere else. The
error also printed the OS error twice.

- `process::find_connection_file` resolves the argument as
  `jupyter_client.find_connection_file` does, without its globbing: a path
  that exists is used as given; a bare name is tried as
  `<runtime_dir>/<name>` and `<runtime_dir>/kernel-<name>.json`.
  `Engine::attach` uses it.
- `--existing` with no value attaches to the newest `kernel-*.json`, as
  `jupyter console --existing` does.
- `main` prints the error chain with `: ` but skips a cause whose text the
  error before it already contains. Our `thiserror` types print their
  source inline (`io: {0}`), so anyhow's `{:#}` repeated it.
