# The API by example

Ganymede serves `jupyter_server`'s REST and websocket API, so its
[REST reference](https://jupyter-server.readthedocs.io/en/latest/developers/rest-api.html)
and the [messaging docs](https://jupyter-client.readthedocs.io/en/latest/messaging.html)
apply. This page shows real exchanges, captured from `ganymede serve`
with ipykernel, so you can see the shapes without running a proxy. Where
Ganymede adds something `jupyter_server` doesn't have, the example says
so.

The examples use a server started like this:

```sh
ganymede serve --root-dir ~/notebooks --port 8888 --token tok
```

## Authenticate

Every request carries the token, in a header or as `?token=`:

```sh
curl -H 'Authorization: token tok' http://127.0.0.1:8888/api/status
```

Without it the API answers `403`. A browser opening a page with
`?token=` is logged in with a cookie and redirected without the token:

```http
GET /lab?token=tok

HTTP/1.1 302 Found
location: /lab
set-cookie: username-localhost-8888=2|1:0|10:…; expires=…; HttpOnly; Path=/; SameSite=Lax
```

API paths and `/files/` and `/nbconvert/` downloads keep the token, so
`curl 'http://127.0.0.1:8888/files/data.csv?token=tok'` gets the file.

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release, which redirects `/files/` too.

Requests that authenticate with that cookie and change state must also
send the `_xsrf` cookie's value as `X-XSRFToken`.
[Security](security.md#tokens-and-cookies) has the rules.

## List kernelspecs

```http
GET /api/kernelspecs
```

```json
{
  "default": "python3",
  "kernelspecs": {
    "python3": {
      "name": "python3",
      "spec": {
        "argv": ["/usr/bin/python3", "-Xfrozen_modules=off", "-m", "ipykernel_launcher", "-f", "{connection_file}"],
        "display_name": "Python 3 (ipykernel)",
        "language": "python",
        "interrupt_mode": "signal",
        "env": {},
        "metadata": { "debugger": true, "supported_encryption": ["curve"] },
        "kernel_protocol_version": "5.5"
      },
      "resources": {
        "logo-32x32": "/kernelspecs/python3/logo-32x32.png",
        "logo-64x64": "/kernelspecs/python3/logo-64x64.png",
        "logo-svg": "/kernelspecs/python3/logo-svg.svg"
      }
    }
  }
}
```

`spec` is the kernelspec's `kernel.json`, with `metadata` passed through
untouched. `default` is `--default-kernel`, or else the first kernelspec
found.

## Start a kernel

```http
POST /api/kernels
Content-Type: application/json

{"name": "python3"}
```

```http
HTTP/1.1 201 Created
location: /api/kernels/5b140482-3d46-486f-8a0c-637e047675f2

{"id": "5b140482-3d46-486f-8a0c-637e047675f2", "name": "python3",
 "last_activity": "2026-10-19T09:04:46.461678Z", "execution_state": "starting", "connections": 0}
```

The reply comes as soon as the kernel process is running, as it does from
`jupyter_server`. The kernel then has `--startup-timeout` seconds to answer;
connect to its websocket meanwhile and watch for `status: idle`. A kernel
that never answers is launched again, up to three times, and is then
reported `status: dead` and removed. `POST /api/sessions` does the same
for a notebook path, and is what JupyterLab and VS Code call.

## Run code over the websocket

Connect to `/api/kernels/<id>/channels?session_id=<your session>` and
send a message with a `channel` key:

```json
{
  "channel": "shell",
  "header": { "msg_id": "req-1", "session": "demo-session", "username": "demo",
              "date": "2026-10-19T09:05:13.051Z", "msg_type": "execute_request", "version": "5.4" },
  "parent_header": {}, "metadata": {},
  "content": { "code": "6 * 7", "silent": false, "store_history": true,
               "user_expressions": {}, "allow_stdin": false, "stop_on_error": true }
}
```

The kernel answers with `status: busy`, `execute_input`, the outputs,
`execute_reply` and `status: idle`. Each carries the request's header as
its `parent_header`. Here is the result:

```json
{
  "channel": "iopub",
  "header": { "msg_id": "00b305a8…_21", "session": "00b305a8…", "username": "username",
              "date": "2026-10-19T09:05:28.823482Z", "msg_type": "execute_result", "version": "5.4" },
  "parent_header": { "msg_id": "req-1", "session": "demo-session", "msg_type": "execute_request", "…": "…" },
  "metadata": {},
  "content": { "data": { "text/plain": "42" }, "metadata": {}, "execution_count": 2 },
  "buffers": []
}
```

iopub messages go to every client connected to the kernel; the
`execute_reply` goes only to the session that sent the request.
[Running a cell](architecture.md#running-a-cell) draws the sequence.
Clients that offer the `v1.kernel.websocket.jupyter.org` subprotocol get
the same messages in its binary framing instead.

## Open a terminal

`POST /api/terminals` starts a shell in a pseudo-terminal and returns its
name:

```http
POST /api/terminals
{"cwd": "projects"}

200 OK
{"name": "1", "last_activity": "2026-10-31T21:02:11.402317Z"}
```

`cwd` is optional. A relative one is resolved against the root dir, and a
missing directory falls back to the root dir. `name` is optional too;
unnamed terminals are numbered from 1, and a name in use is a 409.
`GET /api/terminals` lists them, `GET /api/terminals/<name>` gets one, and
`DELETE /api/terminals/<name>` ends its shell (204).

Connect to `/terminals/websocket/<name>`. Messages are JSON arrays in text
frames, terminado's protocol, which JupyterLab's terminal speaks:

| Direction | Message |
|---|---|
| server → client | `["setup", {}]` first, then the terminal's recent output as one `["stdout", text]` |
| server → client | `["stdout", text]` as the shell writes |
| server → client | `["disconnect", 1]` when the shell exits or the terminal is deleted |
| client → server | `["stdin", text]`, keystrokes |
| client → server | `["set_size", rows, cols]`, the window size |

Any number of clients can attach to one terminal; all see its output. The
shell is `$SHELL` started as a login shell (`powershell.exe` on Windows),
or `terminal_shell` from the [config file](reference.md#config-file).
`terminals_enabled = false` turns the routes off, and they answer 404.
There are no terminals on iOS, where an app cannot start processes.

## Save a notebook

```http
PUT /api/contents/demo.ipynb
Content-Type: application/json

{"type": "notebook", "content": {"cells": [ … ], "metadata": {}, "nbformat": 4, "nbformat_minor": 5}}
```

```http
HTTP/1.1 201 Created
etag: "3f40fb236a2d50da8a8d8648aaa6234dda0aa27fb49c1fe7b2bd98197fe194e3"
location: /api/contents/demo.ipynb

{"name": "demo.ipynb", "path": "demo.ipynb", "type": "notebook",
 "created": "2026-10-19T09:04:53.601383Z", "last_modified": "2026-10-19T09:04:53.601383Z",
 "content": null, "format": null, "mimetype": null, "writable": true, "size": 216,
 "hash": null, "hash_algorithm": null}
```

A save of an existing file answers `200`. To make a save fail rather than
overwrite a file changed on disk since you read it, send back what you
read; both checks are Ganymede's, not `jupyter_server`'s, and a client
that sends neither saves as it would there. With the model's
`last_modified` in the body, a file more than 500 ms newer gets `409`:

```http
HTTP/1.1 409 Conflict

{"message": "conflict: demo.ipynb changed on disk at 2026-10-19T09:05:04.841306Z after the client's copy from 2026-10-19T09:04:54.841383Z", "reason": null}
```

With the `ETag` of a `GET` or a save as `If-Match`, a file whose content
changed gets `412`:

```http
HTTP/1.1 412 Precondition Failed

{"message": "precondition failed: demo.ipynb: content changed, sha256 is now 173c438f…", "reason": null}
```

[Saving a notebook](architecture.md#saving-a-notebook) draws the whole
path.

## Download a file

```http
GET /files/images/plot.png?download=1
```

```http
HTTP/1.1 200 OK
content-type: image/png
content-disposition: attachment; filename*=utf-8''plot.png
content-security-policy: sandbox allow-scripts

<the file's bytes>
```

`/files/{path}` serves a file's raw bytes, as `jupyter_server` does.
JupyterLab uses it for Download and Open in New Browser Tab in the file
browser, and for relative images and links in Markdown. The path is a
contents path, with the same root and hidden-file rules as
`/api/contents`. Without `download`, there is no `Content-Disposition`. A
directory gets `400`.

Every response carries `Content-Security-Policy: sandbox allow-scripts`,
as jupyter_server's does. An HTML or SVG file opened from `/files` runs
its scripts in an opaque origin, so it can't read the UI's token or
storage.

## Check memory and CPU

Ganymede answers [jupyter-resource-usage](https://github.com/jupyter-server/jupyter-resource-usage)'s
two endpoints, which `jupyter_server` has only with that extension
installed. The server's own, counting its child processes, which
include subprocess kernels:

```http
GET /api/metrics/v1
```

```json
{"rss": 104857600, "limits": {"memory": {"rss": 0, "pss": 0}},
 "cpu_percent": 3.2, "cpu_count": 8}
```

One kernel's:

```http
GET /api/metrics/v1/kernel_usage/get_usage/5b140482-3d46-486f-8a0c-637e047675f2
```

```json
{"kernel_id": "5b140482-3d46-486f-8a0c-637e047675f2",
 "content": {"pid": 41234, "kernel_cpu": 0.0, "kernel_memory": 61865984, "cpu_count": 8,
   "hostname": "studio", "host_virtual_memory": {"total": 34359738368, "available": 18253611008,
   "percent": 46.9, "used": 16106127360, "free": 2147483648}, "host_usage_flag": true}}
```

Memory is resident bytes. CPU is the percent of one core since the
server last measured, so it reads 0 the first time and can pass 100 on
several cores. Requests less than sysinfo's minimum CPU interval apart
(200 ms on most systems) share one measurement.

A kernel that answers `usage_request` on its control channel, as
ipykernel does, is asked first, and its whole `usage_reply` message comes
back with `kernel_id` added: a kernel can hold memory outside its own
process (cdp-kernel runs code in a separate browser target). One that
doesn't answer within two seconds is measured from the host, its process
and that process's descendants, and asked again after a minute; one that
replies with an error is measured from the host until it restarts. A kernel the host has
no process for, an attached or in-process one, gets
`{"content": {"reason": "no_process"}, "kernel_id": ...}`, and one that
answered before but not within ten seconds now gets `"reason": "timeout"`.

## Read a kernel's log

When a kernel dies, the reason is usually in its stderr, which no
Jupyter API carries. This route is a Ganymede extension (`jupyter_server`
answers it 404, so a client can probe for it): the kernel process's
stdout and stderr, outside its cell output, and the server's lines about
the process.

```http
GET /api/kernels/5b140482-3d46-486f-8a0c-637e047675f2/log?after=0&lines=200
```

```json
{"kernel_id": "5b140482-3d46-486f-8a0c-637e047675f2",
 "lines": [
   {"seq": 1, "time": "2026-11-04T16:41:02.118230Z", "source": "host",
    "text": "started process 41234: python -m ipykernel_launcher -f /run/kernel-5b14.json"},
   {"seq": 2, "time": "2026-11-04T16:41:09.502117Z", "source": "stderr",
    "text": "MemoryError"},
   {"seq": 3, "time": "2026-11-04T16:41:09.517902Z", "source": "host",
    "text": "exited with code 1"},
   {"seq": 4, "time": "2026-11-04T16:41:09.518033Z", "source": "host",
    "text": "the kernel died; restarting it (1/5)"}],
 "last_seq": 4}
```

`source` is `stdout`, `stderr` or `host`. The log lasts across
restarts and keeps its last 1000 lines or 256 KiB, with longer lines cut
at 8 KiB. After a kernel is shut down or left dead, its log stays
readable under its id for the last 32 such kernels, so a client can
read why it died.

Without `after`, `lines` (default 200, at most 1000) returns the newest
lines. With `after`, it returns the oldest lines whose `seq` is above
it, so `after=0` starts from the oldest line kept. To follow the log,
pass the `seq` of the last line you got as `after` (0 before any); when
a page ends before `last_seq`, ask again. A kernel the
server runs no process for, an in-process one or one on a gateway, gets
`{"kernel_id": ..., "lines": [], "last_seq": 0, "reason": "no_process"}`.

## Every route

What `ganymede serve` answers, beyond the examples above:

- `jupyter_server`'s: `/api` (public), `/api/status`, `/api/me`,
  `/api/config/{section}`, `/api/nbconvert`, `/api/events/subscribe`,
  `/login`, `/logout`, `/api/kernelspecs[/{name}]`,
  `/kernelspecs/{name}/{file}`, `/api/kernels` with `interrupt`,
  `restart` and the `channels` websocket, `/api/sessions`,
  `/api/contents/{path}` with checkpoints and `trust`, `/files/{path}`,
  `/nbconvert/{format}/{path}` for `script`, `markdown` and `html`
  ([Export](frontends.md#export)),
  `/api/terminals` and `/terminals/websocket/{name}`.
- Extensions' routes, with no Python behind them: `jupyter-lsp`'s
  `/lsp/status` and `/lsp/ws/{name}`; `jupyter-collaboration`'s
  `/api/collaboration` rooms with `/api/fileid/*`, and
  `jupyter-server-documents`' `/api/kernels/{id}/execute`
  ([Live editing and document rooms](rooms.md)); `jupyter-resource-usage`'s
  `/api/metrics/v1`.
- Ganymede's own: `/api/kernels/{id}/log`, `/api/notebooks/run`
  ([Parameters and named results](reference.md#parameters-and-named-results)),
  `/api/host/grants/{path}` ([The host API](reference.md#the-host-api)),
  and `/mcp` with `--mcp` ([Agents over MCP](mcp.md)).
- With JupyterLab (`ganymede lab`): the page, `/static/lab`,
  `/lab/extensions/{name}`, and `/lab/api/` `settings`, `workspaces`,
  `translations`, `themes`, `listings`, `plugins`, `licenses`.

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release: `/nbconvert/{format}/{path}`,
> `/api/notebooks/run` and `/api/host/grants`. In the latest release, `/api/nbconvert` reports
> no exporters.

The kernel websocket speaks `v1.kernel.websocket.jupyter.org` when the
client offers it, and the legacy JSON framing otherwise. JSON bodies are
parsed whatever their `Content-Type`, because `@jupyterlab/services`
omits the header on some requests. [Security](security.md) has how
requests are authenticated and which `Host` and `Origin` are accepted.
