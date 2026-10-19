## 2026-10-19, docs restructure (#177)

Step 4 of the docs review. The sidebar now runs Start here, Guides,
Reference, How it works, Background, Contributing: Security is a guide,
Known gaps sits with the reference pages, and Other Jupyter clients and
Prior art moved to a collapsed Background group, since they are context
rather than anything a user acts on.

- Flags, config keys, environment variables, logging and exit codes were
  spread over Getting started, the README and the guides, each copy a
  little different. They now live on one page, `reference.md`, checked
  against `ServeArgs`, `Config` and the CLI's global flags; the guides
  link to it. Only `gateway_url`/`gateway_token` differ from v0.3.0.
- An inline "(unreleased)" in a table is invisible to the release's
  `grep 'Unreleased\.'`, so unreleased rows get a note under their
  section instead.
- The README was 257 lines and shipped in every release archive with the
  crate table, testing and release details that the docs already had.
  Those moved to Architecture, Development and Releasing; the README
  keeps the pitch, install, a short command list and links.
- Protocol's "What is usually missed" read as advice to a future
  implementer. It is now a table with Ganymede's status on each item.
- The where-kernels-run diagram showed four identical-looking stacks.
  Thomas found the differences invisible, so it is now a grid: one row per
  setup, one column per role (UI, Ganymede, kernels, notebooks), color for
  where each piece runs, and a dashed frame around what shares the app or
  the browser tab. The differences are now the shape of each row.
