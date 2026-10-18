## 2026-10-18, login redirect with control characters (#50)

`safe_next` refused `:`, `\` and a leading `//` in the `next` parameter
but let tab, CR and LF through. Browsers remove those from a URL before
parsing it, so `Location: /\t/evil.example` lands on `//evil.example`, a
protocol-relative URL on another host. `/login` redirects at once when the
browser is already logged in or auth is off, so one link was enough. This
is the class of bug jupyter_server fixed for CVE-2023-39968.

- `next` with any control character or whitespace now falls back to the
  base URL, before the other checks run. Checking for `//` alone is not
  enough when the browser rewrites the string afterwards.
- A value that starts with a single `/` followed by anything else is a
  path on the same host however the browser normalizes it, so the checks
  that remain (no scheme, no backslash, no leading `//`, under
  `base_url`) are sufficient once the string is what the browser sees.
