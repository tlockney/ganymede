## 2026-10-31, nb-cli against `ganymede serve` (#240)

[nb-cli](https://github.com/jupyter-ai-contrib/nb-cli) 0.0.10, the notebook CLI meant for agents, works against `ganymede serve` with no server changes, on cdp-kernel 0.7.1 and ipykernel. [`validation/nb-cli.sh`](../../validation/nb-cli.sh) covers it; the results are in [Other Jupyter clients](../clients.md).

In remote mode nb-cli calls `POST /api/fileid/index` (jupyter-server-documents) and `PUT /api/collaboration/session` (jupyter-collaboration); both 404, so it reports "direct (no collaboration backend)" and edits through the contents API: each `nb cell add` is a GET and a PUT of the whole notebook. `nb execute` makes or reuses a session for the notebook path, so separate `nb execute` calls share one kernel and its state, which is what journaling needs. Outputs are written into the file on the server.

Gateway mode (`nb execute --gateway URL --gateway-token T`) keeps the notebook on local disk and uses only `/api/kernels` and the websocket; both `token` and `Bearer` schemes are accepted.

None of what follows is a Ganymede bug; it is nb-cli behaviour an agent (and the Agents page, #241) has to know:

- `nb connect` with no arguments runs `jupyter server list`, which needs Python. Use `--server` and `--token`. #242's `jpserver-<pid>.json` plus an nb-cli change would make auto-detect work.
- `nb create` looks the kernel up on local disk even in remote mode, and only in `VIRTUAL_ENV` and the platform data dirs, not `JUPYTER_PATH` or `JUPYTER_DATA_DIR`. With a kernel installed only where the server runs, `nb create` fails; the script copies the server's kernelspec under a `VIRTUAL_ENV`.
- Gateway mode without `--kernel-id` takes the first kernel listed, whatever its language: a TypeScript notebook ran on a Python kernel.
- A cell timeout (`-t`) does not interrupt the kernel, so the next call waits for the old cell to finish.
- `nb execute` exits 0 when a cell raises; use `nb search --with-errors`.
- stdin is off (`input()` raises `StdinNotImplementedError`), as in nbclient.
- cdp-kernel's `text/plain` results carry ANSI colour codes (Deno-style formatting), which nb-cli passes through to the agent.
