## 2026-11-13, publish options on the HTML export (#754)

Thomas wants to publish from the apps, which are API clients. The
server already rendered the published page at `/nbconvert/html/{path}`,
with its redactions, but only with the defaults. It now takes `nb
publish`'s options as `code`, `byline` and `copy` query parameters.

Choices:

- The existing route rather than a new `/api/notebooks/publish`: it
  already reads the notebook, redacts and sets headers, and a new route
  would be new surface for the same page.
- `copy`, not `download`, for the notebook copy: `download=true` already
  means "send it as an attachment" on every nbconvert route.
- No option keeps the old page (folded, no copy), so JupyterLab's
  Save and Export Notebook As > HTML is unchanged. The apps pass the
  CLI's defaults themselves (copy on unless code is hidden).
- Bad values are a 400 for HTML; the other formats ignore the options.
- The page gets its own policy, `sandbox allow-scripts allow-downloads`
  (the reviewer found the Download notebook button did nothing under
  `/files`' `sandbox allow-scripts`). Output frames keep their own
  sandbox, so a notebook's HTML still can't download.
- One parsing rule for all three: trimmed, case-insensitive, empty is
  absent.
- Every page names its code mode in `<meta name="ganymede-code">`.
  ui#578's review found that a server without these options (0.10.0,
  which the apps embed) returns the folded page for `code=hidden`, so a
  client asking to hide code would share it. The client now refuses a
  page whose mode isn't the one it asked for.
