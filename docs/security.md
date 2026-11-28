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
| `--token` (`token`) | A random token per start | Printed in the URL on stdout. Every API and websocket request needs it, or the login cookie it sets (or, when set up, a [tailnet identity](#tailnet-identity)). |
| `--allow-remote-access` (`allow_remote_access`) | Off on a loopback `--ip`, on otherwise | Off, requests must be addressed to a loopback address or a name in `local_hostnames` (default `localhost`). |
| `--allow-origin` (`allow_origins`) | None | Only the server's own origin may make browser requests. Other origins, once listed, need the token unless `--allow-credentials` is set. |
| `trust_xheaders` | `false` | `X-Forwarded-Proto` and `X-Scheme` are ignored. |
| TLS | None | Ganymede speaks plain HTTP. Put TLS in front of it ([below](#tls)). |

Responses add `frame-ancestors 'self'` to their
`Content-Security-Policy`, as `jupyter_server`'s do, so only the
server's own pages can frame `/lab` or any other page. Another site
can't load it in a hidden frame and steer your clicks. To embed Ganymede
in another site, list that origin in `frame_ancestors`
([reference](reference.md)); `static_headers` can also set its own
`frame-ancestors` for a static page.

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
- `mcp_token`, when set, is a second token that only `/mcp` accepts,
  with no login cookie, for agents ([MCP](mcp.md#a-token-for-agents-only)).
  It keeps an agent off the REST API only while the agent can't run
  code and no output shows the server's token: a cell can read it as any
  kernel code can.

- The cookie signing key is `jupyter_cookie_secret` in the Jupyter
  runtime directory, created owner-only on first start (the file
  `jupyter_server` uses). Delete it to log every browser out.
- Requests that change state and authenticate with the cookie rather
  than the token must carry the `_xsrf` token, and a cross-origin one is
  refused. Kernel connection files, which hold each kernel's HMAC key,
  are written owner-only.
- A terminal (`/api/terminals`) is a shell as the user running the
  server. That is no more than the token already grants through
  kernels, but it needs no kernel installed. `terminals_enabled = false`
  turns terminals off.
- Language servers (`/lsp/*`) run only the commands in the config file;
  a request names a configured server and can't supply a command.
  `/lsp/status` reports each server's spec with `argv` and `env`
  blanked. Notebook text sent to a server is written under
  `.virtual_documents/` in the root dir, and nowhere else.
- Treat the URL with the token like a password. Anyone who sees it (a
  shared terminal, a screenshot, shell history) can run code as you. A
  token set with `--token` or in the config file stays valid across
  restarts. Don't set it in a file others can read.
- One use of a leaked token is enough to keep access: the login cookie
  it gets lasts 30 days, and its signing key survives a restart, so a
  restart with a new token doesn't log that browser out. To cut off
  everyone, delete `jupyter_cookie_secret` and restart with a new token.
  `jupyter_server` behaves the same.

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
  required unless you list tailnet logins, as in
  [Tailnet identity](#tailnet-identity).
- Caddy, nginx, or another reverse proxy works the same way. Serve it at
  a path with `--base-url /jupyter/` if the proxy shares a host.

Behind a proxy, these settings matter:

- **The `Host` check.** A proxy that passes on the `Host` its clients
  used (the tailnet or public name) gets 403 from a loopback server,
  because that name isn't `localhost`. Add the name to `local_hostnames`
  in the config file, so other names are still refused, or pass
  `--allow-remote-access`. Prefer passing `Host` on: a proxy that
  rewrites it to `127.0.0.1:<port>` gets past the `Host` check, but then
  the browser's `Origin` (`https://<name>`) no longer matches `Host`, so
  saves and kernel connections are refused until the origin is in
  `allow_origins`, as in `jupyter_server`.
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

## Tailnet identity

Behind `tailscale serve`, the token can give way to who you are on the
tailnet. `tailscale serve` adds the sender's login name to each request
it forwards (`Tailscale-User-Login`). List the logins to let in:

```toml
ip = "127.0.0.1"
port = 8888
local_hostnames = ["localhost", "workstation.example-tailnet.ts.net"]
trust_xheaders = true

[tailscale]
allow = ["you@example.com"]
```

Then a request with no token or login cookie gets in when it comes from
a loopback address and names a listed login. Everything else is as
before:

- **Off by default.** An empty list lets no one in this way, and there
  is no value that lets in every tailnet user. Entries are whole login
  names. Tags (`tag:server`) need [`whois`](#on-the-tailnet-address),
  because `tailscale serve` sends no identity for tagged devices.
- **The token still works**, for scripts, the apps' remotes and agents,
  and `/login` still takes it. The server refuses `allow` with auth off
  (`token = ""`), where it would keep no one out.
- **Only from the server's own pages.** Requests that change state need
  the `_xsrf` token. A request whose `Origin` isn't the server's own is
  refused, even for origins in `allow_origins` (those still work with
  the token), and so is another site's `<script>`, image or other
  subresource request (by its `Sec-Fetch-Site` header). A link from
  another site still opens the page. So another site's page can't use
  your identity.
- **The `Host` check stays on.** With `allow` set, `allow_remote_access`
  is off unless you turn it on, even with a non-loopback `--ip`, and the
  server refuses to start if you do: a DNS-rebinding page could
  otherwise reach it through `tailscale serve` as you. Put the tailnet
  name in `local_hostnames`, as above.
- **`/mcp` still needs a token** (`mcp_token` or the server's).
- **Only `tailscale serve` may reach the port.** The header is believed
  because the request arrives on loopback, where `tailscale serve`
  connects from. Anything else that arrives there can send it too, so
  the port must be reached only through `tailscale serve`'s HTTPS
  proxy, which sets the header itself. Not through another reverse proxy
  (Caddy, nginx), TCP-mode forwarding (`tailscale serve --tcp`,
  `tailscale funnel --tcp`), or an `ssh -L` tunnel: each passes a
  client's own header through. (`tailscale funnel` in HTTP mode drops
  the header, so a funnel never carries an identity.)
- **HTTPS only.** The server also needs the browser's `Sec-Fetch-Site`
  header to tell its own pages' requests from another site's `<script>`
  or image, and browsers send it only over HTTPS. Over
  `tailscale serve --http` the identity is ignored; use the token there.
  A program that isn't a browser sends a header instead, as below.
- **Sandboxed HTML files lose their images.** Ganymede serves HTML from
  `/files/` and exported notebooks in a sandbox, so their images and
  styles count as another site's requests and get no identity. Open them
  with the token, or sign in with it at `/login`, when they need those.
- **One user per machine.** Any other process on the machine can also
  connect to loopback and send the header. On a machine other people
  log in to, keep the token.
- **The page gets no token.** JupyterLab loaded this way authenticates
  each request by identity, so removing a login from `allow` takes
  effect once the server restarts. A listed login can run code, though,
  and so read the token from the server's files; to cut one off for
  sure, restart with a new token and delete `jupyter_cookie_secret`. Logging out doesn't sign a listed
  login out.
- **Named by identity.** A person who comes with an allowed identity is
  named by their login (a tagged device by its tags), even when they
  also send the token or hold the login cookie: `/api/me` returns it, so JupyterLab shows
  it, and edits in a shared notebook are credited to it whatever name
  the frontend gives. The server log records it once, at info level, the
  first time it is seen, and on each request's span at debug level
  (`-v`).
- Entries must match the login exactly, case included, and must be
  ASCII: `tailscale serve` encodes other characters, so such a login
  would never match.

### Programs

An app, a script or another server reaching Ganymede by tailnet
identity, rather than a browser, sends a `Ganymede-Client` header with
any value (its name, say) and no `Origin`. A page on another site can't
send that header without the browser asking the server first, which it
refuses, so the header also stands in for the `_xsrf` token. Without it,
a request with no `Sec-Fetch-Site` gets no identity. A websocket
handshake may also carry an `Origin` for the server's own address, as
some libraries add, alongside the header.

```sh
curl -H 'Ganymede-Client: curl' https://workstation.example-tailnet.ts.net/api/kernels
```

Ganymede's own [gateway](reference.md#gateway) sends it, so
`ganymede serve --gateway-url` pointed at a Ganymede server that lets
this machine in by tailnet identity needs no `--gateway-token`.

### On the tailnet address

With `whois = true`, the server can listen on its tailnet address
instead of behind `tailscale serve`, and asks `tailscaled` who owns the
address each request comes from (`tailscale whois`). WireGuard
authenticates the address, so another device can't claim one that isn't
its own, and this covers tagged devices, such as a machine running
agents:

```toml
ip = "100.101.102.103"          # this machine's tailnet address (tailscale ip -4)
port = 8888
local_hostnames = ["localhost", "workstation", "workstation.example-tailnet.ts.net"]

[tailscale]
allow = ["you@example.com", "tag:agent"]
whois = true
# cli = "/Applications/Tailscale.app/Contents/MacOS/Tailscale"   # Mac App Store app
```

- **`ip` must be this machine's tailnet address**; the server refuses to
  start otherwise. Only other tailnet addresses (100.64.0.0/10 and
  fd7a:115c:a1e0::/48) are looked up. The server's own address never
  is: a process on this machine that connects to it comes from it, and
  `tailscale whois` would name the machine's owner.
- **Identity is per device.** A tagged device is known by its tags only,
  any other by its owner's login. Anything that reaches the server
  through a device gets that device's identity: everyone using a shared
  tagged machine, or the network behind a subnet router.
- If `tailscale whois` fails or takes over five seconds, the request
  gets no identity.
- **Answers are kept for a minute**, so a device removed from the
  tailnet, or given other tags, may keep its access that long. Failed
  lookups are retried after ten seconds.
- **Plain HTTP, so programs only.** Browsers send no `Sec-Fetch-Site`
  over plain HTTP, so they get no identity this way: use
  `tailscale serve` for JupyterLab. Programs send `Ganymede-Client`, as
  above. The traffic is still encrypted by WireGuard.
- **The `Host` check stays on.** The server accepts its own `ip` in
  `Host`; add its tailnet names to `local_hostnames`.
- Requests from loopback still go through the `tailscale serve` rules,
  so both can be used at once.

## Cross-origin clients

A web page on another origin (a notebook UI hosted elsewhere, Thebe on a
docs site) can use the API only when its origin is listed with
`--allow-origin` (or `allow_origins`). It authenticates with the token,
in an `Authorization: token` header: the browser does not send the login
cookie on its requests, because the server does not send
`Access-Control-Allow-Credentials`. That is `jupyter_server`'s default too.
`--allow-origin '*'` allows every origin, so any page you visit can make
requests, but only with the token. Prefer listing exact origins.

`--allow-credentials` (`allow_credentials`) lets pages on the listed
origins use the login cookie instead, as `jupyter_server`'s
`allow_credentials` does. Those requests must still carry the `_xsrf`
token. The cookie is `SameSite=Lax`, so browsers send it only from a page
on the same site, such as `app.example.org` calling
`notebooks.example.org`. It is ignored while `*` is allowed, since every
page you visit could then act with your session.

## Connected machines

An app that embeds the server can mount other Ganymede servers under
`/machines/{id}/` ([Embedding](embedding.md#connected-machines)). The
mount adds the machine's token to what it forwards, so it serves only the
app's own pages:

- A request must carry this server's own login cookie or its token, and
  come from a loopback peer: a cookie is not enough from anywhere else,
  since any request with the token is given one. A tailnet identity (`[tailscale] allow`)
  gets 403, so a login the tailnet lets into this server gets nothing on a
  machine behind it. The MCP token gets 403 too, so an agent can't reach a
  machine through this server; agents use the machine's own server. With
  auth off, nobody is the app's own session, and the mount answers 403.
- A request with the cookie is checked as the REST API checks it: the
  `Origin` must be this server's or listed, and a request that changes
  state needs the `_xsrf` token.
- Only an allowlist of routes is forwarded, and a path is refused if it
  has `..`, an encoded or raw slash or backslash, an empty segment or a
  NUL, so a page can't name `mcp` or `api/host` on the machine by
  climbing out of an allowed path.
- The one part of `api/host` the allowlist has is `api/host/grants`, a
  notebook's access settings. It adds nothing: the mount already forwards
  `api/kernels`, `api/terminals` and `api/notebooks/run` with the
  machine's token, so a page that may call the mount can already run code
  on the machine. The rest of `api/host` is the cells' own host API, which
  a machine's kernels call on the machine, and stays refused. A machine's
  own pages hold no token and run in the sandbox, so a notebook there
  can't set its own grant through the mount.
- The page's `Cookie`, token, `Origin`, `Referer` and `_xsrf` are removed
  before forwarding, so no local credential reaches the machine (and its
  tailnet `same_origin` check passes).
- Only a short list of the machine's response headers is passed on, so its
  `Set-Cookie` (its login cookie, on a request with its token) never
  reaches the page.
- A machine's HTML lands on the app's origin, so every forwarded response
  carries this server's sandbox policy, whatever the route, not the
  machine's.

The machine's token is held by the server and is never given to the page.
A page the machine serves can run only in the sandbox; it can't act as the
app's UI. A machine you mount is trusted with what the mount sends it: the
page's requests to its allowlisted routes, with your token.

## Kernel providers

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.

A kernel provider is a program of the same user that registers
kernelspecs with a `ganymede serve` and runs their kernels
([Embedding](embedding.md#kernel-providers)). A registered kernel sees
every cell any client runs on it, and its replies become the outputs, so
who may register matters more than who may call the API. The channel is
a Unix socket, not a route, because the API's token reaches browsers and
a loopback check doesn't exclude a proxy:

- Off by default: `[kernel_providers] enabled = true` opens it. A server
  started by hand has no provider. The background server the Mac app
  will start ([#965](https://github.com/tlockney/ganymede/issues/965))
  is to turn it on for the app's kernel.
- Unix only. On Windows the server doesn't open the socket; with
  `enabled` set it warns and serves as usual.
- The socket file is `0600` and lives in the Jupyter runtime dir, which
  the server makes `0700` if it is missing and then checks has no group
  or other write bit and is owned by this user. It is made in a private
  temp dir inside that dir, `chmod`ed, then renamed into place, so it is
  never visible with a wider mode. A runtime dir another user can write,
  or another user owns, means no socket, with a warning: in such a dir
  the socket's own mode would mean nothing.
- The peer's uid must be this server's, read from the connection, and
  the first frame must carry the server's token, compared in constant
  time. Another user's connection is closed with no frame; a wrong
  token gets `welcome` with an error and is closed.
- A provider's spec is display only: `name`, `display_name`, `language`
  and `metadata`. `argv`, `env` and everything else are dropped, so a
  spec from the socket never names a program to run. A name that a
  kernelspec on disk or a kernel server lists is refused, so a provider
  can't stand in front of another kernel.
- A reserved name (`[kernel_providers] reserve`) is refused to every
  provider whose code signature doesn't meet the reservation's
  requirement. On macOS the server reads the peer's audit token from the
  socket, which names the connecting process (not its pid, which another
  process can reuse), and checks that process's signature against the
  `csreq` requirement. The name is refused the same way when the
  signature can't be read, the requirement doesn't parse, or the platform
  is not macOS, so a reservation fails closed; a name that isn't reserved
  is first come, as before. A requirement should pin the team
  (`certificate leaf[subject.OU]`): an `identifier` alone is met by any
  binary signed, even ad hoc, with that identifier. The token is read
  when the server accepts the connection, so a process that hands its
  end of the socket to one that then runs a program meeting the
  requirement may pass as that program; such a process is of the same
  user, which this boundary already trusts with the socket.
- When the connection closes its specs are forgotten and its kernels end
  dead with reason `lost`, with no restart, so a later launch of the name
  goes to whatever lists it then, a kernelspec installed on disk
  afterwards included.

With auth off (`token = ""`), anyone who can reach the port can run
code, so there is nothing a provider check would add: the socket stays
closed, with a warning. An embedded server opens none either; the app's
own kernels go through the C ABI.

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
