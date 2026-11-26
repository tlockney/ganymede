# Brief: forward a machine's notebook access settings through the mount (#972)

Read issue #972 first (`gh issue view 972`). Branch
`claude/project-thread-fneunc`. Keep the repo's comment style (short doc
comments that say why, citing `(#972)`). Delete this file in your
commit. Commit once, do not push.

## Facts

- `crates/ganymede-gateway/src/mount.rs`: `route(rest)` is the
  allowlist (a `match` on decoded segments). Its doc comment lists what
  is allowed and says "`mcp` and `api/host` included" are not. The test
  `the_allowlist_is_exact` lists allowed and refused paths
  (`api/host/grants/a.ipynb` is in the refused list today).
- `crates/ganymede-gateway/tests/mount.rs`: integration tests that mount
  a fake machine and check what is forwarded. Follow an existing GET
  test and an existing state-changing (PUT/POST) test.
- The server's grant routes: `crates/ganymede-server/src/routes.rs`
  (`/api/host/grants/{*path}`): GET, PUT, DELETE. docs/reference.md and
  docs/host-api.md describe them.
- Docs that list the mount's allowlist: docs/embedding.md (around
  "`mcp`, `api/host/*` and everything else on the machine are never
  reachable this way") and docs/security.md (around "so a page can't
  name `mcp` or `api/host` on the machine").

## Changes

1. `route`: allow `["api", "host", "grants", ..]` as `Kind::Other`
   (any depth below, including `api/host/grants` itself). Everything
   else under `api/host` stays `NotAllowed`. Update the doc comment.
2. `the_allowlist_is_exact`: move `api/host/grants/a.ipynb` to the
   allowed list; add `api/host/grants`, `api/host/grants/a/b%20c.ipynb`
   as allowed; add `api/host`, `api/host/`, `api/host/x`,
   `api/host/grantsx`, `api/host/grants/..` (must still be Malformed) as
   refused.
3. Integration test(s) in tests/mount.rs: a GET and a PUT to
   `/machines/{id}/api/host/grants/a.ipynb` reach the machine with the
   method and body intact; `/machines/{id}/api/host/x` is refused (404
   or whatever the existing refused tests expect).
4. Docs: docs/embedding.md and docs/security.md say `api/host/grants`
   (a notebook's access settings) is forwarded, the rest of `api/host`
   is not, and why: the mount already forwards kernels and terminals
   with the same token, and a machine's pages hold no token. Put this
   note next to each change:
   ```
   > [!NOTE]
   > **Unreleased.** Available only from a build of `main` (HEAD), not in
   > the latest release.
   ```
5. Dev log docs/notes/2026-11-26-mount-host-grants.md, starting
   `## 2026-11-26, A machine's notebook access through the mount (#972)`.

## Checks before you commit

`cargo fmt --all -- --check`, `cargo clippy -p ganymede-gateway --all-targets -- -D warnings`,
`cargo test -p ganymede-gateway`, and the repo's docs checks if
CONTRIBUTING.md names them (for example `scripts/check-notes`). Report
the counts.
