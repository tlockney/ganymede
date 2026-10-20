# Known gaps

What Ganymede doesn't do yet, or won't do, in one place. Each item links
to the page with the details. The [roadmap](roadmap.md) says what comes
next, and the troubleshooting step of [Install](install.md)
covers problems with the install itself.

## Platforms

- **Windows** binaries are built and released but not tested end to end
  ([install them](install.md#on-windows)).
  Signal interrupts and killing a kernel process are not implemented
  there, so `interrupt_mode: signal` kernels can't be interrupted and a
  kernel may outlive its shutdown.
- **Linux** binaries need glibc 2.35 or newer. There is no musl build for
  Alpine; [install from source](install.md#from-source) there.
- **macOS** binaries are not signed or notarized. A browser download is
  quarantined; [Install](install.md#macos-blocks-the-binary) has the
  `xattr` fix.

## Frontends

- JupyterLab **server extensions**, which are Python, don't run. Prebuilt
  frontend extensions do; see [JupyterLab and VS Code](frontends.md#extensions).
- No **terminals** and no language server endpoints (`/lsp/*`).
- `/api/nbconvert` reports no exporters: there is no export to HTML, PDF,
  or other formats.
- No JupyterHub single-user mode.
- **Widgets** work in JupyterLab: `ganymede lab install` adds the
  frontends of `jupyterlab_widgets` and [`anywidget`](https://github.com/manzt/anywidget), so ipywidgets from
  ipykernel and cdp-kernel's widgets both render. Widget libraries with
  their own JupyterLab extension need it installed with
  `ganymede lab extension install`.
- **Large output** isn't truncated or rate-limited by the kernel's
  message rate. A client that falls 10,000 output messages behind loses
  the rest of that output and is told, as `jupyter_server`'s rate limit
  does; replies are never dropped.

## Kernels

- In the latest release, a kernel that crashes is reported and removed,
  not restarted automatically as `jupyter_server`'s restarter does. A
  build of `main` restarts it ([Kernel crashes](reference.md#kernel-crashes)).
- Kernels are found only in the platform Jupyter directories and
  `$JUPYTER_PATH`, not under a Python `sys.prefix`. For a kernel installed
  into a conda or virtual environment, add its prefix with
  `--kernelspec-root`; see [Protocol and kernel host](protocol.md).
- The **debugger** (`debug_request` on the control channel) is passed
  through to the kernel untouched, so JupyterLab's debugger should work as far
  as the kernel supports it, but it is untested.
- Conformance tests don't yet cover debugger messages, malformed stdin
  frames, or kernel death and parallel starts through `serve`.

## Notebooks

- nbformat 1 and 2 notebooks can't be read. nbformat 3 notebooks are
  upgraded to 4.5 when opened and saved as 4.5, as Jupyter does.
- A notebook changed on disk while it is open is not reloaded into the
  frontend; that is the document model on the [roadmap](roadmap.md).

## The app

- The Mac and iPad app isn't signed for the App Store or released: the
  Mac build comes from CI, and the iPad build from source; see
  [Ganymede's app](app.md#the-mac-and-ipad-app).
- The iPad runs only cdp-kernel's TypeScript kernel on the device; other
  kernels need a kernel server.

## Embedding

- `ganymede-engine` builds for iOS, Android, and wasm (WASI), but there is
  no runtime seam for running it in a browser page yet.
- The ZeroMQ transport has no CurveZMQ encryption; see
  [Security](security.md) for exposing a server beyond your machine.
