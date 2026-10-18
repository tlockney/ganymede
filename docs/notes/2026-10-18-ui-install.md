## 2026-10-18, `ganymede ui install` and `ganymede ui` (#148)

`ganymede ui install` fetches a ganymede-ui release the way `lab install`
fetches JupyterLab, and `ganymede ui` serves it as `ganymede lab` does,
reusing `lab`'s download agent, size limits, staged install and
`lab_start` (with `require_lab` off, so the URL is `/`).

- The release zip unpacks to one `ganymede-ui-X.Y.Z/` directory; the
  install drops that level so `ui/<version>/index.html` is the app.
- The default version's SHA-256 is pinned in the binary, as JupyterLab's
  wheel is. Other versions take the hash from the release's `SHA256SUMS`,
  which comes from the same place as the zip: it catches corruption, not
  a replaced release. ganymede-ui also publishes a provenance attestation
  per archive; checking it is left for later.
- GitHub answers a release download with a 302 to
  `objects.githubusercontent.com`. Behind this container's proxy that 302
  came back with `Transfer-Encoding: chunked` and an empty body, which
  ureq failed to drain when following the redirect ("chunk length cannot
  be read as a number"). The install follows redirects by hand, checking
  each host, and never reads a redirect's body.
- `GANYMEDE_E2E_UI=1` (set in CI's e2e job) runs the real download in
  `tests/ui.rs`; the rest of that file is offline.
