## 2026-11-01, Language servers in job objects on Windows (#263)

- `child.kill()` on Windows ends one process. An npm-installed language
  server runs as `cmd.exe` running a `.cmd` shim, so killing the child
  left `node` running. Language servers now start suspended, go into a
  kill-on-close job object, and the job is terminated when the server
  stops, as kernels' are.
- The job code stays in `ganymede-kernel-host` (`process::job`, now
  public) and `ganymede-lsp` depends on the kernel host only on Windows,
  with default features off, rather than copying the unsafe Win32 code.
- Windows still has no CI runner (#60). `cargo clippy --target
  x86_64-pc-windows-gnu -p ganymede-lsp -p ganymede-kernel-host` checks
  the Windows-only code from Linux; the server crate needs a MinGW C
  compiler for `ring`, so it can't be checked that way.
