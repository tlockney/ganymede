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
process is needed, only its files. Point `--static-dir` at the bundle of an
installed JupyterLab:

```sh
ganymede serve --static-dir <prefix>/share/jupyter/lab/static
```

The page is served at `/lab`, with the `/lab/api/*` endpoints it calls.
Prebuilt extensions are found in `labextensions` under the Jupyter data
directories or with `--labextensions-path`.

JupyterLab in a browser authenticates its websockets with the login cookie
`jupyter_server` sets, which Ganymede doesn't set yet, so notebooks open
but kernels can't connect. Cookie login is the next step on the roadmap.

## What isn't there

JupyterLab server extensions, terminals, and the language server endpoints
(`/lsp/*`) are Python and are not implemented. `/api/nbconvert` reports no
exporters.

## Other clients

[Other Jupyter clients](clients.md) covers mobile, desktop, editor,
embeddable, and headless clients, and which of them should work.
