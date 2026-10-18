# Security policy

Ganymede is a server that runs code: anyone who can use its API can run
anything the user running it can. Its defaults keep it on the local
machine and behind a token. The
[Security](https://ganymede-server.com/docs/security) page describes the
defaults and what changes when you expose it.

## Supported versions

Ganymede is pre-1.0. Fixes go into the next release, and only the latest
release is supported. Upgrade with the install script or download the
latest release; see [Install](docs/install.md).

## Reporting a vulnerability

Please don't open a public issue for a vulnerability. Report it
privately through GitHub:
[Report a vulnerability](https://github.com/tlockney/ganymede/security/advisories/new)
(the repository's **Security** tab, then **Report a vulnerability**).

Include what you ran (version, `ganymede info`, the command line or
config file), what you expected, what happened, and the requests or
steps that reproduce it. We aim to reply within a week. Once the
problem is understood, we'll agree on a fix and a disclosure date with
you. Advisories credit the reporter unless you'd rather not be named.

In scope: the server (authentication, the Host, Origin, and XSRF checks,
path handling in the contents API, websockets), the kernel host
(connection files, process handling), the CLI's downloads (JupyterLab
and extensions from PyPI), and the release and install tooling.

Out of scope: what a kernel or a notebook's code does once you run it
(that is the point of a kernel), and JupyterLab extensions you choose to
install (they run with your session's access; see the Security page).
