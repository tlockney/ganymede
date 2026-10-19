# Security

`ganymede serve` and `ganymede lab` start a server that runs code.
Anyone who can use its API can start a kernel and run anything you can
run, read and write any file you can, and reach anything your machine can
reach. The contents API is confined to `--root-dir`, but a kernel isn't.
So the question for any deployment is who can reach the server and prove
they hold the token.

The defaults answer "only you, on this machine". This page lists them,
what each setting changes, and how to expose a server safely. The
protections match `jupyter_server`'s, so its security model applies here.

## Defaults

| Setting | Default | Effect |
| --- | --- | --- |
| `--ip` (`ip`) | `127.0.0.1` | Listens on loopback only. Other machines can't connect. |
| `--token` (`token`) | A random token per start | Printed in the URL on stdout. Every API and websocket request needs it, or the login cookie it sets. |
| `--allow-remote-access` (`allow_remote_access`) | Off on a loopback `--ip`, on otherwise | Off, requests must be addressed to a loopback address or a name in `local_hostnames` (default `localhost`). |
| `--allow-origin` (`allow_origins`) | None | Only the server's own origin may make browser requests that use the login cookie. |
| `trust_xheaders` | `false` | `X-Forwarded-Proto` and `X-Scheme` are ignored. |
| TLS | None | Ganymede speaks plain HTTP. Put TLS in front of it ([below](#tls)). |

## Tokens and cookies

- The token is checked in an `Authorization: token <t>` (or `Bearer`)
  header or a `?token=` parameter, compared in constant time. Request
  logs replace a `token` parameter with `[secret]`.
- A request that sends the right token gets a signed login cookie, so a
  browser opened from the printed URL stays logged in. It is
  `HttpOnly` and `SameSite=Lax`, is scoped to `--base-url`, and lasts 30
  days. It is marked `Secure` when the request came over HTTPS, which
  Ganymede can only know through a proxy and `trust_xheaders`. Without a
  cookie or token, a page redirects to `/login`, which asks for the
  token. `/logout` clears the cookie.
- The cookie signing key is `jupyter_cookie_secret` in the Jupyter
  runtime directory, created owner-only on first start (the file
  `jupyter_server` uses). Delete it to log every browser out.
- Requests that change state and authenticate with the cookie rather
  than the token must carry the `_xsrf` token, and a cross-origin one is
  refused. Kernel connection files, which hold each kernel's HMAC key,
  are written owner-only.
- Treat the URL with the token like a password. Anyone who sees it (a
  shared terminal, a screenshot, shell history) can run code as you until
  the server restarts with a new token. A token set with `--token` or in
  the config file stays valid across restarts. Don't set it in a file
  others can read.

### `--token ""`

An empty token turns authentication off: anyone who can connect can run
code. The Host and XSRF checks still apply, as in `jupyter_server`, so
a web page you visit cannot drive a loopback server through your browser.
But any local user, and any process on the machine, can connect. Use it
only where the port is reachable by you alone, such as inside a container
whose port isn't published. The server logs a warning at start.

## Listening beyond loopback

`--ip 0.0.0.0` (or `::`, or a LAN address) listens on every interface,
which exposes the server to everyone on every network the machine joins:
the office LAN, café Wi-Fi, a VPN. It also turns `allow_remote_access` on,
since requests will come addressed to names Ganymede can't know. What
stands between those networks and running code is then the token alone,
sent in plain text by every request, where anyone on the path can read
it.

So don't use `--ip 0.0.0.0` on its own. Instead:

![Three ways to reach Ganymede from another machine. Avoid listening on every interface with --ip 0.0.0.0: requests travel as plain HTTP, so anyone on the network can read the token. Instead forward a port over SSH to a server listening on 127.0.0.1, or put a TLS proxy such as tailscale serve, Caddy or nginx in front of a server on 127.0.0.1, with local_hostnames and trust_xheaders set. Each needs the token.](images/architecture/security.svg)

- **On one machine**, keep the default and open the printed URL.
- **From another machine of yours**, forward a port over SSH and leave
  the server on loopback:

  ```sh
  ssh -L 8888:127.0.0.1:8888 workstation   # then open the URL locally
  ```

- **For a tailnet or LAN**, keep Ganymede on loopback and put a
  TLS-terminating proxy in front of it, as below.

## TLS

Ganymede has no TLS of its own. Terminate TLS in a proxy on the same
machine and point it at the loopback port:

- [`tailscale serve`](https://tailscale.com/kb/1312/serve) serves a
  local port to your tailnet over HTTPS with a certificate for the
  machine's tailnet name:

  ```sh
  ganymede serve --root-dir ~/notebooks --port 8888
  tailscale serve --bg 8888
  ```

  Only devices on your tailnet can connect, and the token is still
  required.
- Caddy, nginx, or another reverse proxy works the same way. Serve it at
  a path with `--base-url /jupyter/` if the proxy shares a host.

Behind a proxy, two settings matter:

- **The `Host` check.** A proxy that passes on the `Host` its clients
  used (the tailnet or public name) gets 403 from a loopback server,
  because that name isn't `localhost`. Add the name to `local_hostnames`
  in the config file, so other names are still refused, or pass
  `--allow-remote-access`. A proxy that rewrites `Host` to
  `127.0.0.1:<port>` needs neither.
- **`trust_xheaders = true`** in the config file, when the proxy sends
  `X-Forwarded-Proto: https`, so the login cookie is marked `Secure`.
  Set it only when every request comes through a proxy you control:
  otherwise a client can send the header itself.

```toml
# ganymede.toml, for `ganymede serve --config ganymede.toml` behind a TLS proxy
ip = "127.0.0.1"
port = 8888
local_hostnames = ["localhost", "workstation.example-tailnet.ts.net"]
trust_xheaders = true
```

## Cross-origin clients

A web page on another origin (a notebook UI hosted elsewhere, Thebe on a
docs site) can use the API only when its origin is listed with
`--allow-origin` (or `allow_origins`), and it still needs the token.
`--allow-origin '*'` allows every origin. Combined with the login cookie,
that lets any page you visit act with your session. Prefer listing exact
origins.

## JupyterLab extensions are code

A JupyterLab extension is JavaScript that JupyterLab loads into its page.
It runs on the server's origin with your login cookie, so it can do
anything you can do in JupyterLab: start kernels, run code, read and
write files. `ganymede lab extension install` downloads prebuilt
extensions from PyPI (and their Jupyter extension dependencies, at their
latest versions), and `--labextensions-path` and the Jupyter data
directories add more. Install only extensions you would trust with a
shell. `ganymede lab install` checks JupyterLab's wheel against the
SHA-256 PyPI publishes, and against a pinned hash for the default
version.

Notebook trust is separate: a notebook's HTML and JavaScript outputs run
in JupyterLab only once the notebook is trusted, signed with the key in
`--notary-dir`, as Jupyter does.

## Reporting a problem

Report security problems privately through GitHub's
[Report a vulnerability](https://github.com/tlockney/ganymede/security/advisories/new)
form, not in a public issue.
[SECURITY.md](https://github.com/tlockney/ganymede/blob/main/SECURITY.md)
has what to include and which versions are supported.
