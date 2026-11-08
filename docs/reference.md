# CLI and configuration

Every command, the server's flags and config file, environment
variables, logging, and exit codes. `ganymede <command> --help` prints the
same flags. The guides show them in use:
[Getting started](getting-started.md),
[JupyterLab and VS Code](frontends.md), and [Ganymede's app](app.md).

## Commands

| Command | Does |
| --- | --- |
| `kernelspec list`, `paths`, `install <dir>`, `remove <name>` | Find and manage kernelspecs, as `jupyter kernelspec` |
| `run` | Run code as cells and print the output |
| `console` | An interactive console on a kernel |
| `execute` (`exec`) | Run notebooks top to bottom, as `jupyter execute` |
| `nb clear` | Remove a notebook's outputs |
| `trust` | Sign a notebook as trusted, as `jupyter trust` |
| `kernel` | Start a kernel and print its connection file, as `jupyter kernel` |
| `attach` | Run code on a running kernel by its connection file |
| `serve` | Serve the Jupyter REST and websocket API |
| `lab` | `serve` with JupyterLab, opened in the browser |
| `lab install`, `list`, `remove`, `clean`, `uninstall` | Manage JupyterLab bundles |
| `lab extension install`, `list`, `enable`, `disable`, `remove` | Manage prebuilt JupyterLab extensions |
| `ui`, `ui install`, `list`, `remove` | `serve` with [ganymede-ui](app.md#in-a-browser-from-ganymede) |
| `mcp`, `mcp call` | Notebook tools for coding agents over MCP on stdio, or one tool call from a script; see [Agents over MCP](mcp.md) |
| `info` | Versions and the directories in use |
| `completions <shell>` | Shell completions for bash, zsh, fish, powershell, elvish |

The kernel name `fake` is a built-in in-process kernel for trying things
out.

## Flags for every command

| Flag | Effect |
| --- | --- |
| `--kernelspec-root <dir>` | Search this directory (one holding `kernels/`) before the Jupyter data dirs. Repeatable. |
| `--startup-timeout <secs>` | Seconds a new kernel has to answer. Default 60. |
| `-q`, `--quiet` | Discard kernel processes' own stdout and stderr. Cell output is unaffected. |
| `-v`, `--verbose` | Report kernel details and per-cell status on stderr; for `serve`, `lab` and `ui`, log at debug level. |

## Server flags

`serve`, `lab` and `ui` take these. Each sets the config file key of the
same name in snake case, except where the last column says otherwise.

| Flag | Effect | Config key |
| --- | --- | --- |
| `--root-dir` | Where notebooks and files are served from. Default: the current directory. | |
| `--ip` | Address to listen on. Default `127.0.0.1`; a non-loopback address also turns on `--allow-remote-access`. | |
| `--port` | Default 8888. `0` picks a free port. `lab` and `ui` try up to 50 more while it is taken. | |
| `--token` | The auth token. Unset generates one; an empty string disables auth. | |
| `--allow-remote-access` | Accept requests whose `Host` is not a loopback address or a name in `local_hostnames`. | |
| `--allow-origin` | An origin, besides the server's own, allowed to use the API and websockets. Repeatable. | `allow_origins` |
| `--allow-credentials` | Let pages on the `--allow-origin` origins use the login cookie. Off by default; ignored when `*` is allowed. See [Security](security.md#cross-origin-clients). | `allow_credentials` |
| `--base-url` | A prefix for serving behind a reverse proxy, e.g. `/jupyter/`. Leading and trailing slashes are added when missing. | |
| `--cookie-secret-file` | Key for signing login cookies. Default: `jupyter_cookie_secret` in the Jupyter runtime dir. | |
| `--default-kernel` | The kernelspec new notebooks start. Default: the first kernelspec found. | |
| `--cull-idle-secs` | Shut down kernels idle this long with no clients. `0` (the default) never does. | |
| `--kernel-restart-limit` | Restart a kernel that crashes, up to this many times in a row, before leaving it dead. Default `5`; `0` never restarts. See [Kernel crashes](#kernel-crashes). | `kernel_restart_limit` |
| `--checkpoint-dir` | Keep checkpoints here instead of `.ipynb_checkpoints` beside each file. | |
| `--notary-dir` | Where the notebook-signing key and trusted signatures live. Default: the Jupyter data dir. | |
| `--static-dir` | Serve a frontend's files. JupyterLab's bundle is served at `/lab`, anything else at `/`. Default: the newest JupyterLab from `ganymede lab install`. | |
| `--lab-version` | Which installed JupyterLab to serve. | |
| `--labextensions-path` | Another directory of prebuilt JupyterLab extensions, searched first. Repeatable. | `labextensions_paths` |
| `--config-dir` | Where frontend settings and workspaces are kept. Default: the Jupyter config dir. | |
| `--gateway-url`, `--gateway-token` | Run kernels on another server; see [Gateway](#gateway). | |
| `--kernelspec-root` | As above. | `kernelspec_roots` |
| `--startup-timeout` | As above. | `startup_timeout_secs` |
| `-q`, `--quiet` | As above. | `quiet` |
| `--mcp` | Serve notebook tools for coding agents at `/mcp`, behind the token; see [Agents over MCP](mcp.md). | `mcp_enabled` |
| `--config <file>` | The config file to read. | |
| `--log-level`, `--log-file` | See [Logging](#logging). | |

`serve` prints the URL, with its token, as the only line on stdout.
[Security](security.md) covers what `--ip 0.0.0.0` and `--token ""`
expose.

## Config file

A TOML file of the keys above, passed with `--config`. Without it,
`ganymede.toml` in the Jupyter config dir is read if it exists. Flags
override the file, and an unknown key is an error, so a misspelt `token`
can't start a server with a random one.

```toml
root_dir = "/srv/notebooks"
port = 9000
default_kernel = "cdp-node"
cull_idle_secs = 3600
allow_origins = ["https://notebooks.example.org"]
```

Every key, with its default:

```toml
# ganymede.toml: every key, set to its default or commented out.
# Flags given on the command line override these.
ip = "127.0.0.1"
port = 8888
# token = "..."                  # unset: a new random token each start; "" turns auth off
root_dir = "."                   # the current directory
# checkpoint_dir = "/path"       # unset: .ipynb_checkpoints beside each file
# notary_dir = "/path"           # unset: the Jupyter data dir
# cookie_secret_file = "/path"   # unset: jupyter_cookie_secret in the runtime dir
# static_dir = "/path"           # unset: the newest JupyterLab from `ganymede lab install`
# lab_version = "4.6.4"
labextensions_paths = []
# config_dir = "/path"           # unset: the Jupyter config dir
base_url = "/"
kernelspec_roots = []
# default_kernel = "cdp-node"    # unset: the first kernelspec found
cull_idle_secs = 0               # 0: never
kernel_restart_limit = 5         # 0: a crashed kernel stays dead
startup_timeout_secs = 60
quiet = false
allow_origins = []
allow_credentials = false
# allow_remote_access = true     # unset: on when ip isn't a loopback address
local_hostnames = ["localhost"]
trust_xheaders = false
max_body_size = 536870912        # 512 MiB
# gateway_url = "https://kernels.example.org"
# gateway_token = "..."
terminals_enabled = true         # false: no /api/terminals
terminal_shell = []              # empty: $SHELL -l, or powershell.exe on Windows
mcp_enabled = false              # true: notebook tools for agents at /mcp
mcp_read_only = false            # /mcp reads notebooks only
mcp_no_execute = false           # /mcp edits notebooks but runs no code
mcp_images = false               # /mcp sends images in outputs to the model
crash_reporting = false          # true: send crash reports (see Privacy)
frame_ancestors = "'self'"       # who may frame the server's pages

# Extra response headers for files from a static_dir that
# isn't JupyterLab, none by default (see Static headers). One table per path; "/assets/*" matches
# every path under /assets/:
# [static_headers."/kernel.html"]
# Cross-Origin-Opener-Policy = "same-origin"
# Cross-Origin-Embedder-Policy = "require-corp"

# Language servers for jupyterlab-lsp, none by default. One table each:
# [language_servers.pyright]
# argv = ["pyright-langserver", "--stdio"]
# languages = ["python"]
# mime_types = ["text/x-python"]
```

These five keys have no flag. The terminal keys, the MCP keys other
than `mcp_enabled`, `static_headers` and `language_servers` have none
either; the sections below and [Agents over MCP](mcp.md#options) cover
them.

| Key | Effect |
| --- | --- |
| `local_hostnames` | Host names accepted besides loopback addresses while remote access is off. Default `["localhost"]`. |
| `trust_xheaders` | Trust `X-Forwarded-Proto` and `X-Scheme` from a reverse proxy, so login cookies sent over proxied HTTPS are marked `Secure`. |
| `max_body_size` | Largest request body, in bytes, which caps a notebook's size. Also the largest message a [document room](rooms.md#connections) takes, and how many bytes of messages a room client may fall behind before it is closed, so lowering it to cap uploads also closes slow room clients sooner. Default 512 MiB. |
| `crash_reporting` | Send crash reports from `ganymede` to Sentry; see [Privacy and crash reporting](privacy.md). Default `false`. |
| `frame_ancestors` | The CSP sources allowed to frame the server's pages, added to every response as `frame-ancestors <sources>`. Default `'self'`, as `jupyter_server`; add an origin, such as `'self' https://portal.example.com`, to embed `/lab` in that site. See [Security](security.md#defaults). |

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.

## Static headers

`static_headers` adds response headers to files served from a
`static_dir` that isn't JupyterLab. Keys are paths under `base_url`; a
key ending in `*` matches every path that starts with the rest of it.
Keys match the request path, and also the path as the file is found:
percent-decoded, without empty or `.` segments, and with `index.html`
added to one ending in `/`. Nothing
is added by default. Routes the server answers itself, such as
`/api/status`, never get these headers; a request no route claims, such
as an unknown `/api/` path, falls through to the static files and can.
When several keys set the same header, the last key in sorted order
wins, and a configured header replaces one the server set. The
`frame_ancestors` directive is appended to a `Content-Security-Policy`
set here unless it already has a `frame-ancestors` directive, so a page
can set its own.

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.

A page that needs `SharedArrayBuffer`, such as one hosting a
kernel in a web worker, must be cross-origin isolated:

```toml
[static_headers."/kernel.html"]
Cross-Origin-Opener-Policy = "same-origin"
Cross-Origin-Embedder-Policy = "require-corp"
```

An app embedding the server passes the same object in its JSON config
(`"static_headers": {"/kernel.html": {...}}`). The server refuses to
start when a key doesn't begin with `/` or a header name or value isn't
valid HTTP.

## Kernel crashes

A kernel that exits without being shut down or restarted, through the
API or by a client's `shutdown_request`, is restarted under the same id, in the same directory, as `jupyter_server`'s
`KernelRestarter` does. Its clients see:

1. For the request it was running, a reply with `status: error` and
   `ename: KernelDied` (and an `error` output, for a cell); for requests
   queued behind it, `status: aborted` replies. Each is followed by
   `status: idle`, so clients stop waiting for them.
2. `status: restarting`, which JupyterLab shows as an automatic restart
   and reconnects on.
3. The new kernel's own statuses.

A kernel that crashes again within ten seconds of being restarted counts
toward `kernel_restart_limit` (default 5). One crash past the limit, it
is reported `status: dead` and removed, as it always is with the limit
set to `0`. The kernel's state is lost in a restart, as it is under
Jupyter.

## Gateway

`--gateway-url` runs kernels on another Jupyter or Ganymede server while
notebooks and files stay local, as `jupyter_server --gateway-url` does:

```sh
ganymede serve --root-dir . \
  --gateway-url https://kernels.example.org --gateway-token "$TOKEN"
```

![Running kernels on another server. On your machine, a frontend talks to ganymede serve started with --gateway-url, which answers contents, sessions and trust from the notebook folder. For kernelspecs and kernels it calls the kernel server's REST API (kernelspecs, start, interrupt, restart, shut down) and relays each kernel's messages over its websocket. The kernel server, jupyter server or ganymede serve, runs the kernel processes over ZeroMQ.](images/architecture/gateway.svg)

Kernelspecs come from that server (`/api/kernelspecs`, refreshed every
minute); starting, interrupting, restarting and shutting down a kernel go
through its REST API, and kernel messages through its websocket. HTTPS is
checked against the system's trust store.

## Server info file

While it runs, `serve`, `lab` and `ui` write `jpserver-<pid>.json` in the
Jupyter runtime dir, as `jupyter_server` does, so tools that look for a
running Jupyter server find Ganymede too: `jupyter server list` lists it,
and an agent or script can read the URL and token from it instead of being
given them.

```json
{
  "base_url": "/",
  "ganymede_version": "0.7.0",
  "hostname": "127.0.0.1",
  "password": false,
  "pid": 41237,
  "port": 8888,
  "root_dir": "/Users/me/notebooks",
  "secure": false,
  "sock": "",
  "token": "3f9c...",
  "url": "http://127.0.0.1:8888/",
  "version": "2.21.1"
}
```

The keys are `jupyter_server`'s; `version` is the `jupyter_server`
version whose API Ganymede matches (as `GET /api` reports), and
`ganymede_version` tells a client it is Ganymede. `token` is empty when
auth is off. Beside it, `jpserver-<pid>-open.html` redirects a browser to
the server with the token. Both are readable only by you, since they hold
the token, and are removed when the server stops; a file left by a server
that was killed names a process that is gone, which `jupyter server list`
checks and cleans up. A server embedded in an app writes neither.

`lab` and `ui` open the browser on `jpserver-<pid>-open.html`, as
`jupyter_server` does, so the token is not in the browser command's
arguments. Snap and Flatpak browsers, the default Firefox and Chromium on
Ubuntu 22.04 and later, can't read the runtime dir and show an
access-denied page; `--no-redirect-file` opens the URL itself instead, as
`jupyter_server`'s `use_redirect_file = False` does.

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.

## Environment variables

| Variable | Effect |
| --- | --- |
| `JUPYTER_CONFIG_DIR` | The Jupyter config dir. Default `~/.jupyter`. |
| `JUPYTER_DATA_DIR` | The user Jupyter data dir, where kernelspecs and signatures are. |
| `JUPYTER_PATH` | More data dirs to search for kernelspecs, first. |
| `JUPYTER_RUNTIME_DIR` | Where connection files, the cookie secret and the [server info file](#server-info-file) go. Default: `runtime/` in the data dir. |
| `JUPYTER_GATEWAY_URL`, `JUPYTER_GATEWAY_AUTH_TOKEN` | `--gateway-url` and `--gateway-token` when not given. |
| `GANYMEDE_DATA_DIR` | Where `lab install` and `ui install` put what they download. |
| `GANYMEDE_PYPI_URL`, `GANYMEDE_PYPI_HOSTS` | A PyPI mirror, and more hosts allowed to serve wheels; see [What a download trusts](frontends.md#what-a-download-trusts). |
| `GANYMEDE_UI_URL` | A mirror of ganymede-ui's release downloads. |
| `GANYMEDE_CRASH_REPORTING` | `1` turns crash reporting on and `0` off, overriding `crash_reporting`; see [Privacy and crash reporting](privacy.md). |
| `RUST_LOG` | The log filter, when `--log-level` isn't given. |
| `HTTPS_PROXY` | Used for downloads. |

`ganymede info` prints every directory in use.

## Logging

`serve`, `lab` and `ui` log at info level on stderr; `-v` raises it to
debug, and `RUST_LOG` overrides both. `--log-level` (`trace`, `debug`,
`info`, `warn`, `error`, or Jupyter's `WARNING` and `CRITICAL`)
overrides all three, and `--log-file` appends the log to a file instead
of stderr. Kernels' own output lines start with
`[kernel <first 8 of its id>]`.

## Exit status

| Status | Meaning |
| --- | --- |
| 0 | Success |
| 1 | A cell failed, a kernel timed out or died, or a file or download failed (as `jupyter execute`) |
| 2 | Usage error: an unknown flag, a missing argument, no code to run |
| 128+N | Stopped by signal N: 130 for Ctrl-C, 143 for SIGTERM, 129 for SIGHUP |
