# Development log

> [!NOTE]
> This is a history for contributors, not documentation. An entry
> describes the code as it was when the entry was written. For how
> Ganymede works now, read the guides and the reference; site search
> leaves this page out.

Findings from each step, newest last: what was built, what was checked
against the reference implementations, what broke, and what was learned.

The design started as a single `sketch.md`, split on 2026-10-17 into
[Architecture](architecture.md), [Replacing Python](python-free.md),
[Protocol and kernel host](protocol.md), [User interfaces](ui.md),
[The iPad use case](ipados.md), [Roadmap](roadmap.md), and
[Prior art](prior-art.md). Older entries refer to the sketch by section
number, and entries before the rename use the old `jcore-` crate names.

## Adding an entry

Each pull request adds its own file to [`notes/`](notes/), named
`YYYY-MM-DD-<topic>.md` (for example `2026-10-18-request-body-limit.md`),
instead of appending to one shared file, so PRs open at the same time
don't conflict. The file holds one entry that starts with
`## YYYY-MM-DD, <topic> (#<issue>)`. Links are relative to `notes/`
(`../architecture.md`).

The docs site shows every entry on this page: first
`notes/archive.md`, the log before it was split, then
the dated files in name order (see `.vitepress/notes.ts`). On GitHub the
entries are the files in [`notes/`](notes/).
