## 2026-11-09, downloads retry a 5xx or a timeout (#634)

Item 13 of the project read of 2026-11-09 (#629).

The weekly CI run of 2026-11-09 failed in e2e's `install_from_github`:
`ganymede ui install` got an HTTP 504 from a GitHub release download and
gave up. `ganymede lab install` and `lab extension install` fetch from
PyPI through the same agent and gave up the same way.

- `retry` in `crates/ganymede-cli/src/lab/pypi.rs` sends a request again
  after 1, 2 and 4 seconds while it fails with a 5xx status or a ureq
  timeout, and prints a line on stderr each time. The three requests
  that fetch something go through it: PyPI's JSON, a wheel, and a
  release file (each hop of the redirects `ganymede ui install` follows
  by hand).
- Nothing else is retried. A 404, a refused connection, a failed name
  lookup or a TLS error would fail the same way again, and someone
  offline should hear so at once and not seven seconds later.
- It covers the request up to the response's headers. A body that stalls
  or is cut off part way still fails the install: retrying that means
  starting the file again, and the hash check already catches a short
  download. That is the next step if it turns out to matter.
- The worst case is longer than before: four attempts at the 60 second
  wait for a response, where there was one.

The tests run a plain HTTP server on localhost that answers each
connection from a script, so they see the errors ureq really reports for
a 504 and for a server that never answers. The agent the CLI uses is
HTTPS only, so the tests build their own and call the same function.
