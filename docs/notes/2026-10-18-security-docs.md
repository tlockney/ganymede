## 2026-10-18, a security policy and a security page (#65)

Ganymede is a server that runs code, and nothing said so: no
SECURITY.md, and no page with the default bind address, what
`--ip 0.0.0.0` exposes, what `--token ""` leaves in place, or how to put
TLS in front of it.

- `SECURITY.md` at the root: report privately through GitHub's advisory
  form, only the latest release is supported, and what is in scope. A
  kernel running code, or an extension you installed, is not a
  vulnerability.
- [Security](../security.md) in the docs, under Reference: the defaults
  table, tokens and cookies, `--token ""` (Host and XSRF checks still
  apply since #41, but any local process can connect), why not to use
  `--ip 0.0.0.0` on its own, SSH forwarding, TLS through `tailscale
  serve` or a reverse proxy with `local_hostnames` and `trust_xheaders`,
  `--allow-origin '*'`, and JupyterLab extensions as code with your
  session's access.
- `CONTRIBUTING.md`, which points at [development](../development.md), so
  GitHub's Contributing tab isn't empty.

Writing the page meant reading `auth.rs` and `config.rs` claim by claim.
Each setting it describes is one of `jupyter_server`'s, with the same
default.
