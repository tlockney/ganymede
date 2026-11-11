## 2026-11-11, Host API grants over HTTP (#668)

The apps' granting sheet, the last step of the host API plan, runs in
the web UI, which reaches the server only over REST. So
`/api/host/grants/{path}` does what `ganymede grant` does: `GET` answers
the notebook's own grant and the `[host]` defaults, `PUT` replaces the
grant, and `DELETE` (or `PUT {}`) removes it.

The defaults come from a new `AppState.host`, the `[host]` table the
server was started with, so a client can show what a notebook gets
without its own grant. `PUT` checks the keys itself rather than relying
on serde, since `Grant` reads stored grants with `#[serde(default)]` and
would otherwise take a typo as an empty grant. Paths are normalized as
the CLI's are.

The notebook must exist and be a notebook (`index_existing`, under the
move lock, so a deleted path never gets an id). A server with file ids
in memory answers `409`, as `ganymede grant` refuses a store that isn't
persistent. The route sits behind the token like the rest; a kernel has
none, so it can't grant itself anything.
