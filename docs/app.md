# Ganymede's app

[ganymede-ui](https://github.com/tlockney/ganymede-ui) is Ganymede's own
notebook app: JupyterLab's notebook in a smaller, Mac-style window. It is
a client of the same API as JupyterLab and VS Code, so it runs against
Ganymede, against `jupyter_server`, or with no server at all. It is early
software; its [known gaps](https://github.com/tlockney/ganymede-ui/blob/main/docs/notes/archive.md)
are listed in its development log.

There are three ways to run it, and they differ in where notebooks live
and where kernels run:

![Where the pieces run: on a workstation, a frontend talks to ganymede serve, which launches kernel processes; in the Mac app, ganymede-ui in a web view talks to Ganymede inside the app, which launches the Mac's kernels or uses a kernel server; in the iPad app, Ganymede inside the app serves notebooks and cdp-kernel runs in the web view unless a kernel server is set; in a browser with no server, notebooks stay in IndexedDB and kernels run in Workers.](images/architecture/where-kernels-run.svg)

| | Notebooks | Kernels |
| --- | --- | --- |
| [`ganymede ui`](#in-a-browser-from-ganymede) | The folder you serve | Your installed kernelspecs |
| [Mac app](#the-mac-and-ipad-app) | A folder you pick | The Mac's kernelspecs, or a kernel server |
| [iPad app](#the-mac-and-ipad-app) | The app's Documents, or a folder you pick | cdp-kernel in the app, or a kernel server |
| [This browser](#with-no-server) | The browser's storage | cdp-kernel or JavaScript, in the page |

## In a browser, from Ganymede

> [!NOTE]
> **Unreleased.** `ganymede ui` is available only from a build of `main`
> (HEAD), not in the latest release.

```sh
ganymede ui install            # once: the default release, from GitHub
ganymede ui                    # serves it at / and opens it in the browser
ganymede ui list               # installed versions; --json too
ganymede ui remove 0.1.0
ganymede ui install --version 0.2.0
```

`ganymede ui` is `ganymede lab` for this app: `serve` with the newest
installed version as `--static-dir`, the same flags and port fallback,
and the URL of `/` printed and opened. The token in the URL becomes a
login cookie, and the app connects on it.

`ganymede ui install` downloads `ganymede-ui-X.Y.Z.zip` from the release
on GitHub and checks its SHA-256: pinned in the binary for the default
version, otherwise the line in that release's `SHA256SUMS`. It unpacks to
`ui/<version>/` under Ganymede's data directory (`ganymede info` shows
it), within the same size limits as JupyterLab. `GANYMEDE_UI_URL` points
at a mirror laid out as `<url>/v<version>/<file>`.

With the latest release, download the zip from ganymede-ui's
[releases](https://github.com/tlockney/ganymede-ui/releases), unpack it,
and serve it with `ganymede serve --static-dir <dir>`.

## The Mac and iPad app

The app runs Ganymede inside its own process and shows ganymede-ui in a
web view, so there is nothing else to install or start. It is built from
ganymede-ui's [`apple/`](https://github.com/tlockney/ganymede-ui/tree/main/apple)
folder and isn't in the App Store or a release yet:

- **Mac.** Each CI run of ganymede-ui's `macos-app` job uploads
  `Ganymede-macOS`, a zipped build, under the run's artifacts. It is
  ad-hoc signed and not notarized, so clear the quarantine flag before
  the first launch: `xattr -dr com.apple.quarantine Ganymede.app`.
- **iPad and iPhone.** Build it from source with Xcode; the
  [`apple/` README](https://github.com/tlockney/ganymede-ui/tree/main/apple#build)
  has the steps.

On the Mac, File › Open Folder… (⇧⌘O) picks the notebook folder, and
kernels are the kernelspecs installed on the Mac, as with `ganymede
serve`. The app takes `PATH` from your login shell, so kernels that run
`node`, `deno` or `python3` start. Quitting shuts every kernel down.

On the iPad, notebooks live in the app's Documents folder (On My iPad in
the Files app) or a folder you pick, such as one in iCloud Drive. iOS
doesn't let an app start kernel processes, so the page runs cdp-kernel's
TypeScript kernel itself, in a Worker.

### Kernels on another machine

Both apps can run kernels on another Jupyter or Ganymede server instead:
Settings (⌘,) on the Mac, the server button on the iPad. Enter the
server's URL and token. Notebooks stay on the device; only kernels run
there, through Ganymede's
[gateway](getting-started.md#run-kernels-on-another-server). To serve
kernels from a Mac on your network, see [Security](security.md) for
reaching it safely; `ganymede serve --ip 0.0.0.0` alone sends the token
in clear.

## With no server

In the connect sheet, **This browser** keeps notebooks in the browser's
storage (IndexedDB) and runs kernels in Workers in the page: cdp-kernel's
TypeScript kernel, or JupyterLite's JavaScript kernel. Nothing is sent
anywhere. **Ganymede server** with **Run kernels in this page** keeps
notebooks on the server and still runs kernels in the page, which is what
the iPad app does.

## Using it

- **Toolbar.** Run, insert, kernel picker, interrupt and restart, Trust
  Notebook, and Download Notebook.
- **Shortcuts.** JupyterLab's defaults. Press H in command mode, or Help ›
  Keyboard Shortcuts on the Mac, for the list; bindings can be changed
  there and are kept in the browser.
- **Trust.** Outputs of a notebook you didn't run are sanitized until you
  trust it, as in JupyterLab; on Ganymede, trusting signs the file. The
  browser-only mode keeps no signatures.

How the app is built is on ganymede-ui's
[architecture](ganymede-ui/architecture.md) page.
