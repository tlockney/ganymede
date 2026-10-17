# JupyterLab and VS Code

Ganymede serves the same REST and websocket API as `jupyter_server`, so
existing frontends connect to it without changes. Start the server
first:

```sh
ganymede serve --root-dir ~/notebooks --port 8888
```

## VS Code

In the Jupyter extension, choose **Select Kernel**, then **Existing
Jupyter Server**, and paste the URL `ganymede serve` printed, token
included. VS Code lists the kernelspecs Ganymede found; pick one.

The extension's remote-server flow passes against Ganymede: listing
kernels and sessions, starting, interrupting, and restarting a kernel,
stdin, and running cells.

## JupyterLab

Ganymede serves JupyterLab's own static bundle, so no `jupyter lab`
process is needed, only its files. `ganymede lab install` downloads them
from PyPI's `jupyterlab` wheel (no Python involved) and checks its SHA-256;
`ganymede lab` then serves the newest installed version in the current
directory and opens it in the browser, like `jupyter lab`:

```sh
ganymede lab install                    # JupyterLab 4.6.4; --version picks another
ganymede lab list
ganymede lab                            # JupyterLab at /lab, opened in the browser
ganymede lab --no-browser --port 9000   # takes `serve`'s flags
ganymede serve                          # the same at /lab, no browser, API-only if none installed
ganymede serve --lab-version 4.6.4      # a specific installed version
ganymede lab remove 4.6.4
```

Bundles live in `ganymede/lab/<version>/` under the local data directory:
`~/.local/share` (or `$XDG_DATA_HOME`) on Linux, `~/Library/Application
Support` on macOS, `%LOCALAPPDATA%` on Windows, or `$GANYMEDE_DATA_DIR/lab`
when that is set. Each holds JupyterLab's `static/`, `schemas/`, `themes/`,
and its BSD-3-Clause `LICENSE`. The download honours `HTTPS_PROXY` and the
system's trusted certificates. With nothing installed, `serve` serves only
the API.

The bundle of a JupyterLab installed some other way works too:

```sh
ganymede serve --static-dir <prefix>/share/jupyter/lab/static
```

The page is served at `/lab`, with the `/lab/api/*` endpoints it calls.
Prebuilt extensions are found in `labextensions` under the Jupyter data
directories or with `--labextensions-path`.

`ganymede lab` fails when no JupyterLab is installed; `ganymede serve`
serves the API alone. `lab` also moves past a taken port, from 8888 up
to 50 more, as `jupyter lab` does, and prints the URL of `/lab`.

Open the URL `ganymede lab` or `ganymede serve` printed. The token in it becomes a login
cookie, and the browser is redirected to the same page without it; the
cookie is what JupyterLab's websockets authenticate with. Without the
token, the page sends you to `/login`, which asks for it. `/logout` ends
the login.

## What isn't there

JupyterLab server extensions, terminals, and the language server endpoints
(`/lsp/*`) are Python and are not implemented. `/api/nbconvert` reports no
exporters.

## Other clients

[Other Jupyter clients](clients.md) covers mobile, desktop, editor,
embeddable, and headless clients, and which of them should work.
