## 2026-10-18, server config: unknown keys fail, global flags apply (#55)

A config file with `tokn = "x"` or `cull_idle_sec = 30` started a server
with a random token and culling off, and said nothing: `Config` was
`#[serde(default)]` only. `ganymede serve` and `ganymede lab` also listed
the global `-q`, `-v` and `--startup-timeout` in `--help` and ignored
them, and `serve` logged at warn while the `ganymede-server` binary logs
at info.

- `Config` now has `deny_unknown_fields`; the error names the key and the
  file.
- `--startup-timeout` and `-q` map onto `startup_timeout_secs` and a new
  `quiet` key (kernels' own stdout and stderr discarded), so they apply to
  kernels the server launches. Unset, the file's values stand: the global
  flag no longer has a clap default, so "not given" can be told apart.
- `serve` and `lab` log at info by default, debug with `-v`; `RUST_LOG`
  still wins.
- Flags for keys that were TOML-only: `--base-url`, `--checkpoint-dir`,
  `--allow-origin` (repeatable), `--cookie-secret-file`.

Learned: `serve`'s own `--kernelspec-root` shares the global's name, and
clap fills both from one occurrence, so the roots were listed twice.

Left for later: a default config file path, `--log-level` and
`--log-file`, and a kernel-id prefix on kernel output.
