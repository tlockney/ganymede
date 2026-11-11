## 2026-11-11, Playwright jobs survive an apt mirror outage (#686)

From about 14:10 UTC the runners couldn't resolve archive.ubuntu.com,
azure.archive.ubuntu.com or security.ubuntu.com, so `workstation
(ubuntu-latest)` and `lsp` failed in `npx playwright install --with-deps
chromium` before any test ran, on every re-run.

GitHub's Ubuntu image ships Google Chrome, so the libraries Chromium
needs are already there; `--with-deps` mostly adds fonts. Both jobs now
download the browser alone, then run `playwright install-deps chromium`
as best effort: when apt fails they log a warning and carry on with the
image's libraries. During the outage apt took about 10 minutes to give
up (its own DNS retries), so the step is capped at 5 minutes. `timeout`
only stops `npx`: the `sudo apt-get` Playwright started kept running
beside the tests until the runner cleaned up, and would have installed
fonts and held the dpkg lock mid-job had the mirrors come back. A failed
step now stops it with `sudo pkill -x apt-get`.

Retrying apt with a backoff wouldn't have helped: the outage lasted
hours. If a future image drops a library, Chromium fails to launch and
Playwright names what's missing, so the failure stays readable.
