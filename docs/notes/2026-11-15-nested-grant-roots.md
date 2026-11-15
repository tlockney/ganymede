## 2026-11-15, grants of roots inside a renamed or deleted folder (#829)

A server that renamed or deleted a path followed only the roots above it
(`ganymede grant` project directories and other servers), so the grants
of a root at or inside the path were left on a directory that was gone.
A grant with no stamp there would then have taken a file made anew at the
path on its first use. A rename now moves every other root at or under
the old path to the same place under the new one, with its ids, paths and
grants, after dropping what roots at or under the new path hold, which
comes from something deleted outside and is never what moves in. A delete
drops every id and grant of the other roots at or under the path, and so
does a file or directory made by a copy or `new_untitled`, which
forgets stale rows there. Roots are found by reading the distinct roots in the database and
matching whole path segments, so `proj` doesn't match `projx`. The roots
move in one transaction, so a failure moves nothing; it is logged, as
before.
