## 2026-11-07, a base URL without a leading slash (#476)

`--base-url jupyter` panicked at startup: serve only trimmed the trailing
slash, and axum can't nest a router at a path without a leading one.
`normalize_base_url` now adds the leading slash and drops the trailing
one, as `jupyter_server` normalizes `base_url`, so `jupyter`, `/jupyter`
and `/jupyter/` all serve under `/jupyter/`. Found in the 0.7.0 code
review (#440).
