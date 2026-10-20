# Switching from jupyter_server

Ganymede replaces the server, not the kernels. If you run `jupyter lab`
today, your Python notebooks keep running on ipykernel, with pandas,
matplotlib and the rest of your environment, once ipykernel's kernelspec
is installed. What changes is the process between the frontend and the
kernel: one binary instead of a Python environment with
`jupyter_server`, `jupyter_client`, `nbformat` and `traitlets` in it.

This page lists what carries over, what is different, and what is
missing, so you can judge the switch before making it.

## What carries over

| | |
| --- | --- |
| Notebooks | The same `.ipynb` files. Saves match Python's `nbformat` byte for byte, so they don't churn in git. |
| Kernels | Every installed kernelspec in the Jupyter data dirs and `$JUPYTER_PATH`, ipykernel's included. |
| Trust | The same `notebook_secret` and `nbsignatures.db`: a notebook trusted in Jupyter is trusted here. |
| Logins | The same `jupyter_cookie_secret`, so a browser stays logged in. |
| JupyterLab settings | `~/.jupyter/lab/user-settings/` and `workspaces/`. |
| Clients | JupyterLab, VS Code's Jupyter extension, and other clients of the REST and websocket API. See [JupyterLab and VS Code](frontends.md). |
| Headless runs | `ganymede execute` writes the same outputs as `jupyter execute`; `ganymede trust` replaces `jupyter trust`. |

## What is different

- **Installing JupyterLab.** `ganymede lab install` downloads JupyterLab's
  browser files from PyPI without running Python, instead of
  `pip install jupyterlab`. Prebuilt extensions install with
  `ganymede lab extension install <package>`.
- **Kernels in an environment.** `jupyter_client` also finds kernels under
  the Python environment it runs in (`sys.prefix`). Ganymede has no
  Python environment, so a kernel installed into a virtualenv or conda
  environment needs that prefix added:
  `--kernelspec-root "$VIRTUAL_ENV/share/jupyter"`. A kernel installed
  with `--user`, as `python -m ipykernel install --user` does, is found
  as it is.
- **The default kernel** is `--default-kernel`, or else the first
  kernelspec found, not `python3`.
- **A kernel that crashes** is reported and removed, not restarted.
- **Configuration** is one TOML file instead of `jupyter_server_config.py`
  ([CLI and configuration](reference.md#config-file)). The settings that
  carry over:

  | `jupyter_server` | `ganymede.toml` |
  | --- | --- |
  | `ServerApp.ip`, `ServerApp.port` | `ip`, `port` |
  | `IdentityProvider.token` | `token` |
  | `ServerApp.root_dir` | `root_dir` |
  | `ServerApp.base_url` | `base_url` |
  | `ServerApp.allow_origin` | `allow_origins` (a list) |
  | `ServerApp.allow_credentials` | `allow_credentials` |
  | `ServerApp.allow_remote_access`, `ServerApp.local_hostnames` | `allow_remote_access`, `local_hostnames` |
  | `ServerApp.trust_xheaders` | `trust_xheaders` |
  | `ServerApp.cookie_secret_file` | `cookie_secret_file` |
  | `ServerApp.max_body_size` | `max_body_size` |
  | `MappingKernelManager.default_kernel_name` | `default_kernel` |
  | `MappingKernelManager.cull_idle_timeout` | `cull_idle_secs` |
  | `FileCheckpoints.checkpoint_dir` | `checkpoint_dir` |
  | `NotebookNotary.data_dir` | `notary_dir` |
  | `GatewayClient.url`, `GatewayClient.auth_token` | `gateway_url`, `gateway_token` |

  Other settings have no equivalent. An unknown key in the file is an
  error, so a setting that doesn't carry over can't be ignored silently.

## What is missing

Server extensions (the Python half of some JupyterLab extensions),
terminals, the language server endpoints, nbconvert export, JupyterHub's
single-user mode, real-time collaboration, and reloading a notebook
changed on disk. [Known gaps](known-gaps.md) has the full list and the
details. If you depend on one of these, stay on `jupyter_server` for now,
or run it for that one job: both servers share kernels, notebooks and
signatures, so they can run side by side.

## TypeScript notebooks

Ganymede was built for TypeScript notebooks with
[cdp-kernel](https://cdp-kernel.org), with no Python installed at all.
The JavaScript ecosystem for data work is smaller than Python's, and
there is no single library with pandas' reach. If your work is pandas
and matplotlib, keep it on ipykernel; Ganymede runs both kernels side by
side, and you can move notebook by notebook.
