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

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release. The latest release answers only after the kernel's
> first reply, with `execution_state: idle`.

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

<the file's bytes>
```

`/files/{path}` serves a file's raw bytes, as `jupyter_server` does.
JupyterLab uses it for Download and Open in New Browser Tab in the file
browser, and for relative images and links in Markdown. The path is a
contents path, with the same root and hidden-file rules as
`/api/contents`. Without `download`, there is no `Content-Disposition`. A
directory gets `400`.

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.
