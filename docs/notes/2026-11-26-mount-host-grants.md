## 2026-11-26, A machine's notebook access through the mount (#972)

A connected machine's window can now show and change that machine's
notebook access. The mount refused everything under `api/host`, so the
apps had nothing to ask and the window dimmed the section.

`route` allows `api/host/grants` and anything below it, as `Kind::Other`,
and nothing else under `api/host`: the rest is the cells' own host API,
which a machine's kernels call on the machine itself, never through this
Mac. The grant subtree takes `GET`, `PUT` and `DELETE`, since the mount
forwards the method and body as they are; it needed no rule of its own.

This adds nothing a page with the mount can't already do. The mount
forwards `api/kernels`, `api/terminals` and `api/notebooks/run` with the
machine's token, so whoever can call it can run code on the machine, and
a grant is smaller than that. The machine's server still checks its own
token and its own `[host]` defaults, and a grant set through the mount is
the one the machine's own UI sets. A page the machine serves runs in the
sandbox and holds no token, so a notebook can't use the route to widen its
own grant.

`the_allowlist_is_exact` now lists the subtree as allowed and `api/host`,
`api/host/`, `api/host/x` and `api/host/grantsx` as refused; a `..` in it
is still `Malformed`, not merely refused, so `api/host/grants/..` reads as
the same path it always did. The redirect test's examples moved off
`api/host/grants/x`, which is now a path a `Location` may name. A new
mount test sends a GET and a PUT to a stand-in and checks the method, path
and body arrive, and that `api/host/x` never does.
