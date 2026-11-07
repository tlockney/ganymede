## 2026-11-07, an empty `_xsrf` field falls back to the header (#488)

`check_xsrf` took the first source that was present (the form field, the
`_xsrf` query argument, then the `X-XSRFToken` and `X-CSRFToken` headers)
and only then dropped an empty value, so a form with `_xsrf=` was
refused even when the header carried the token. Tornado's
`check_xsrf_cookie` chains the sources with `or`, which skips empty
strings too. Each source is now checked for emptiness before falling
through to the next.
