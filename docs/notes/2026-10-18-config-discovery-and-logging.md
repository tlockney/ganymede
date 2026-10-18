## 2026-10-18, default config file, log flags, kernel output prefix (#55)

What the first #55 fix left for later:

- Without `--config`, `serve`, `lab` and `ganymede-server` read
  `ganymede.toml` from the Jupyter config dir (`$JUPYTER_CONFIG_DIR`, else
  `~/.jupyter`, as `jupyter_config_dir()`) when it exists, so a daemon
  needs no flags. The file read is logged at info. `--config` still wins.
- `--log-level` takes tracing's names and Jupyter's `WARNING` and
  `CRITICAL`, and overrides `-v` and `RUST_LOG`. `--log-file` appends the
  log there, without colors, instead of stderr; stdout keeps only the URL.
  Both are `ServeArgs` flags, so `lab` has them too; the CLI installs the
  subscriber from them before dispatch.
- The kernel host has a `LaunchOptions.prefix_output`: the kernel's
  stdout and stderr are piped and copied to the host's stderr a line at a
  time behind `[kernel <first 8 of id>] `. The kernel manager sets it; the
  CLI's single-kernel commands don't. "launched kernel" now logs the full
  `kernel_id`, so the short prefix can be traced back.

Learned: `jupyter_server` itself has no prefix (kernels inherit its
stderr); the prefix is ours, for a server whose kernels share one log.
