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
| `info` | Versions and the directories in use |
| `completions <shell>` | Shell completions for bash, zsh, fish, powershell, elvish |

> [!NOTE]
> **Unreleased.** `ganymede ui` is available only from a build of `main`
> (HEAD), not in the latest release.

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
| `--base-url` | A prefix for serving behind a reverse proxy, e.g. `/jupyter/`. | |
| `--cookie-secret-file` | Key for signing login cookies. Default: `jupyter_cookie_secret` in the Jupyter runtime dir. | |
| `--default-kernel` | The kernelspec new notebooks start. | |
| `--cull-idle-secs` | Shut down kernels idle this long with no clients. `0` (the default) never does. | |
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
| `--config <file>` | The config file to read. | |
| `--log-level`, `--log-file` | See [Logging](#logging). | |

> [!NOTE]
> **Unreleased.** `--gateway-url` and `--gateway-token` are available only
> from a build of `main` (HEAD), not in the latest release.

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

Three keys have no flag:

| Key | Effect |
| --- | --- |
| `local_hostnames` | Host names accepted besides loopback addresses while remote access is off. Default `["localhost"]`. |
| `trust_xheaders` | Trust `X-Forwarded-Proto` and `X-Scheme` from a reverse proxy, so login cookies sent over proxied HTTPS are marked `Secure`. |
| `max_body_size` | Largest request body, in bytes, which caps a notebook's size. Default 512 MiB. |

## Gateway

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.

`--gateway-url` runs kernels on another Jupyter or Ganymede server while
notebooks and files stay local, as `jupyter_server --gateway-url` does:

```sh
ganymede serve --root-dir . \
  --gateway-url https://kernels.example.org --gateway-token "$TOKEN"
```

Kernelspecs come from that server (`/api/kernelspecs`, refreshed every
minute); starting, interrupting, restarting and shutting down a kernel go
through its REST API, and kernel messages through its websocket. HTTPS is
checked against the system's trust store.

## Environment variables

| Variable | Effect |
| --- | --- |
| `JUPYTER_CONFIG_DIR` | The Jupyter config dir. Default `~/.jupyter`. |
| `JUPYTER_DATA_DIR` | The user Jupyter data dir, where kernelspecs and signatures are. |
| `JUPYTER_PATH` | More data dirs to search for kernelspecs, first. |
| `JUPYTER_RUNTIME_DIR` | Where connection files and the cookie secret go. Default: `runtime/` in the data dir. |
| `JUPYTER_GATEWAY_URL`, `JUPYTER_GATEWAY_AUTH_TOKEN` | `--gateway-url` and `--gateway-token` when not given. |
| `GANYMEDE_DATA_DIR` | Where `lab install` and `ui install` put what they download. |
| `GANYMEDE_PYPI_URL`, `GANYMEDE_PYPI_HOSTS` | A PyPI mirror, and more hosts allowed to serve wheels; see [What a download trusts](frontends.md#what-a-download-trusts). |
| `GANYMEDE_UI_URL` | A mirror of ganymede-ui's release downloads. |
| `RUST_LOG` | The log filter, when `--log-level` isn't given. |
| `HTTPS_PROXY` | Used for downloads. |

> [!NOTE]
> **Unreleased.** `JUPYTER_GATEWAY_URL`, `JUPYTER_GATEWAY_AUTH_TOKEN` and
> `GANYMEDE_UI_URL` are read only by a build of `main` (HEAD), not by the
> latest release.

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
