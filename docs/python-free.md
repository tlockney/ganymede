# Replacing Python

The messaging protocol, the websocket framing, the REST API, and nbformat
are language-neutral. Python shows up around them. Each item below is a
place the reference stack assumes Python; the right column is what
Ganymede does instead. The [architecture](architecture.md) page shows
where these pieces sit.

| Python assumption in the reference stack | Replacement |
|---|---|
| `jupyter --paths`, `sys.prefix/share/jupyter` as a kernelspec search root | Search only the platform dirs: `$JUPYTER_PATH`, `$JUPYTER_DATA_DIR/kernels`, `~/Library/Jupyter/kernels` (macOS), `${XDG_DATA_HOME:-~/.local/share}/jupyter/kernels` (Linux), `/usr/local/share/jupyter/kernels`, `/usr/share/jupyter/kernels`. cdp-kernel writes to exactly these. |
| `jupyter kernelspec list/install/remove` is a Python CLI | `ganymede kernelspec list/paths/install/remove`, in the same binary as the server. |
| Default kernel name is `python3` (`MappingKernelManager.default_kernel_name`, nbformat defaults, sessions API fallback) | No built-in name. `--default-kernel` (or `default_kernel` in the config file) sets one; otherwise it is the first kernelspec found. |
| Kernel provisioners are Python classes loaded by entry point | A Rust trait with subprocess and attach-to-running-kernel implementations, and a remote one, `--gateway-url`, that proxies to another server as `jupyter_server`'s gateway client does. |
| Windows interrupt via a named event in an env var set by `jupyter_client` for ipykernel | Support `interrupt_mode: signal` and `interrupt_mode: message` only. cdp-kernel interrupts over CDP internally; check its generated `kernel.json` for which mode it declares. Windows binaries are built but untested, and signal interrupts don't work there ([Known gaps](known-gaps.md#platforms)). |
| `ipc` transport for connection files | cdp-kernel uses `node:net` TCP only. Default to `tcp`; keep `ipc` as an option for kernels that support it. |
| Widgets mean ipywidgets: Python model classes plus `@jupyter-widgets/controls` | Widgets are the comm protocol plus the `jupyter.widget` state-sync protocol; the Python half is the kernel's business. `ganymede lab install` adds the frontend halves of `jupyterlab_widgets` and [`anywidget`](https://github.com/manzt/anywidget) from PyPI without Python, so cdp-kernel's anywidget-based widgets and ipywidgets from ipykernel both render. |
| Trust signatures in `nbsignatures.db` via Python's sqlite and a secret file | The same files, read and written in Rust: HMAC-SHA256 over the notebook as nbformat computes it, keyed with `notebook_secret`, recorded in `nbsignatures.db` through SQLite compiled into the binary. Notebooks trusted by Jupyter stay trusted. `ganymede trust` replaces `jupyter trust`. |
| nbconvert endpoints (`/nbconvert/*`) and export menus | Drop. Export is a separate tool if wanted. |
| Server extensions via Python entry points, traitlets config, `jupyter_server_config.py` | Drop. One config file (TOML) for the server. No extension system in v1. |
| JupyterHub single-user contract | Drop for v1. It is an auth handshake plus env vars and can be added later without touching the core. |
| Language servers via `jupyter-lsp`, which finds servers with per-language Python spec finders | `/lsp/status` and `/lsp/ws/<name>` in Rust, relaying JSON-RPC to servers listed in the config file. No autodetection, so the host names no language. **Unreleased.** A build of `main` only. |
| Terminals via `terminado` | `/api/terminals` and terminado's websocket protocol in Rust, over a pseudo-terminal (`portable-pty`). The terminal runs the user's login shell, or `terminal_shell` from the config; the host names no language. **Unreleased.** A build of `main` only. |
| Language-specific UI behaviour: `%matplotlib`, `pip` magics, Python tracebacks | None. The only source of language information is `kernel_info_reply.language_info`; the editor mode comes from `codemirror_mode`, which cdp-kernel reports as TypeScript. Traceback rendering is ANSI text, not a Python format. |
| Optional protocol messages treated as required (`history`, `is_complete`, `inspect`) | Treat every message beyond `kernel_info`, `execute`, `shutdown`, `interrupt`, and `status` as optional. Read `supported_features` when present. Time out and degrade rather than hang. |

## Where Python still appears

"No Python" means no Python runtime and no Python dependency. Python
shows up in three places, none of them a dependency:

- `ganymede lab install` unpacks JupyterLab's JavaScript from its PyPI
  wheel without running Python.
- CI runs `jupyter execute`, `jupyter server`, and ipykernel as the
  reference Ganymede is compared against.
- The notebook writer matches the bytes Python's `nbformat` writes, so
  files don't churn in git.

What cdp-kernel needs from the server (a connection file, signing,
stdin, comms with binary buffers, debugger passthrough) is covered in
[Protocol and kernel host](protocol.md), and which frontends work without
Python in [JupyterLab and VS Code](frontends.md).
