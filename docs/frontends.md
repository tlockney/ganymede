# JupyterLab and VS Code

Ganymede serves the same REST and websocket API as `jupyter_server`, so
existing frontends connect to it without changes. Start the server
first:

```sh
ganymede serve --root-dir ~/notebooks --port 8888
```

## VS Code

In the Jupyter extension, choose **Select Kernel**, then **Select Another
Kernel…** › **Existing Jupyter Server**, and paste the URL `ganymede serve`
printed, token included. VS Code lists the kernelspecs Ganymede found; pick
one. The Python extension isn't needed for this;
[Install](install.md#vs-code) gives the sources.

The extension's remote-server flow passes against Ganymede: listing
kernels and sessions, starting, interrupting, and restarting a kernel,
stdin, and running cells.

## JupyterLab

Ganymede serves JupyterLab's own static bundle, so no `jupyter lab`
process is needed, only its files. `ganymede lab install` downloads them
from PyPI's `jupyterlab` wheel (no Python involved) and checks its SHA-256;
`ganymede lab` then serves the newest installed version in the current
directory, like `jupyter lab`:

```sh
ganymede lab install                    # JupyterLab 4.6.4; --version picks another
ganymede lab                            # JupyterLab at /lab, opened in the browser
ganymede lab --no-browser --port 9000   # takes `serve`'s flags
ganymede serve --lab-version 4.6.4      # the same at /lab, without the browser
ganymede lab list                       # installed versions; --json too
ganymede lab remove 4.6.4
ganymede lab clean                      # leftovers of interrupted installs
ganymede lab uninstall                  # every version and extension
```

`ganymede lab` prints the URL of `/lab` and opens it (`open` on macOS,
`xdg-open` on Linux). If port 8888 is taken it tries the next, up to 50
more, as `jupyter lab` does. It fails, naming `ganymede lab install`, when
no JupyterLab is installed; `ganymede serve` then serves only the API.

The token in the URL becomes a login cookie, and the browser is
redirected to the same page without it; the cookie is what JupyterLab's
websockets authenticate with. Without the token, the page sends you to
`/login`, which asks for it. `/logout` ends the login.

Bundles live in `ganymede/lab/<version>/` under the local data directory:
`~/.local/share` (or `$XDG_DATA_HOME`) on Linux, `~/Library/Application
Support` on macOS, `%LOCALAPPDATA%` on Windows, or `$GANYMEDE_DATA_DIR/lab`
when that is set; `ganymede info` prints where. Each holds JupyterLab's
`static/`, `schemas/`, `themes/`, and its BSD-3-Clause `LICENSE`. The
download honours `HTTPS_PROXY` and the system's trusted certificates.

The bundle of a JupyterLab installed some other way works too, served at
`/lab` with the `/lab/api/*` endpoints it calls:

```sh
ganymede serve --static-dir <prefix>/share/jupyter/lab/static
```

Any other `--static-dir` is served at `/`, behind the API: a built
[ganymede-ui](app.md) works this way. Its HTML pages get an `_xsrf`
cookie, as JupyterLab's page does, so an app on the login cookie can send
it back as `X-XSRFToken` on requests that change state.

### Extensions

JupyterLab loads prebuilt extensions at runtime, and packages on PyPI ship
them inside their wheels. `ganymede lab extension` installs them the same
way, without Python:

```sh
ganymede lab extension install jupyterlab-execute-time   # latest; or NAME==VERSION, 'NAME>=1,<2', NAME~=1.2
ganymede lab extension list
ganymede lab extension disable jupyterlab-execute-time   # and `enable`
ganymede lab extension remove jupyterlab-execute-time
```

`disable` writes `labconfig/page_config.json` in the Jupyter config dir,
as `jupyter labextension disable` does, and the next page load picks it
up.

`ganymede lab install` also installs `jupyterlab_widgets` and [`anywidget`](https://github.com/manzt/anywidget),
so widgets (cdp-kernel's `jupyter.widget`, for one) work out of the box;
`--no-default-extensions` skips them.

Only the wheel's `share/jupyter/labextensions/` is unpacked, into
`ganymede/labextensions/` beside the bundles, which `serve` always
searches. Dependencies are followed only as far as the frontend needs:
through requirements of packages classified `Framework :: Jupyter`,
installing those classified `Framework :: Jupyter :: JupyterLab ::
Extensions`. So `anywidget` brings `jupyterlab_widgets` by way of
`ipywidgets`. Each package gets the newest release its PEP 440
specifiers allow (`==`, `!=`, `<`, `<=`, `>`, `>=`, `~=`, `==1.2.*`),
from the command line or from the `Requires-Dist` of the package that
needs it; an installed dependency is replaced only when it no longer
satisfies them. Pre-releases are picked only when a specifier names one
or nothing else matches, and yanked releases never are, even when pinned.
A wheel must be `py3-none-any`. `--no-deps` installs only the named
packages. Server extensions, the Python half of some packages, don't run.

A package can't replace an extension another package installed unless
the command has `--force`, and the install fails on an extension whose
directory name is not an npm package name (`name` or `@scope/name`, not
starting with `.` or `_`).

### What a download trusts

An extension is JavaScript that JupyterLab runs on the server's origin,
so it can do anything you can from the browser: read and write files,
start kernels, and run code in them. Install only packages whose
publishers you trust. The same holds for JupyterLab itself.

Downloads are HTTPS only, from `pypi.org`'s JSON API and wheels on
`files.pythonhosted.org`. A redirect must land on one of those hosts too.
Each wheel's SHA-256 is checked against the value PyPI lists next to its
URL. That catches a corrupted or truncated download. It doesn't catch a
malicious release, which would come with a matching hash; only the
default JupyterLab version's hash is also pinned in the binary.
`ganymede` prints each wheel's URL and SHA-256 before installing it.

Downloads have timeouts and are capped at 256 MB. A wheel can unpack to
at most 100,000 entries, 256 MB per file and 1 GB in all.

To use a mirror of PyPI's JSON API, set `GANYMEDE_PYPI_URL` (for example
`https://mirror.example/pypi`). Wheels may then come from the mirror's
host as well. `GANYMEDE_PYPI_HOSTS` adds more allowed hosts, separated by
commas.

### Extension Manager

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.

JupyterLab's Extension Manager panel lists the installed prebuilt
extensions and whether each is enabled, from `GET /lab/api/extensions`.
It is read-only, as JupyterLab's own `read-only` manager is: searching
finds nothing, and its install, uninstall, enable, and disable buttons
get an error that points at `ganymede lab extension`.

### Other extension directories

Extensions installed some other way are found in `labextensions` under the
Jupyter data directories or with `--labextensions-path`, which is searched
first.

## What isn't there

JupyterLab server extensions, terminals, and the language server endpoints
(`/lsp/*`) are Python and are not implemented. `/api/nbconvert` reports no
exporters.

## Other clients

[Other Jupyter clients](clients.md) covers mobile, desktop, editor,
embeddable, and headless clients, and which of them should work.
