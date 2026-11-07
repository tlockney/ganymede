## 2026-11-07, lab install: only public versions reach PyPI URLs (#432)

`Version::parse` dropped everything after `+`, so a pin like
`==1.0+/../../evil/1.0` parsed, and `Pypi::json` pasted its raw text into
`{api}/{project}/{version}/json`. A `Requires-Dist` entry from PyPI could
use that to point the lookup at another project's metadata.

- `Version::parse` now checks the local part: letters and digits separated
  by `.`, `-`, or `_`. Anything else is not a version.
- `Index::json` refuses a version that isn't public (any `+` part, or a
  character outside letters, digits, `.`, `!`, `-`, `_`) before building
  the URL. PyPI has no local versions, so `==1.0+local` is now an error
  naming the version instead of a request to a wrong URL.
