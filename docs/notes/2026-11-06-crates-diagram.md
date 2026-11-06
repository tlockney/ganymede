## 2026-11-06, the crates diagram gets documents, terminals and lsp (#403)

`crates.svg` predated three crates. Redrawn from each crate's
`Cargo.toml`, leaving out dependencies another arrow implies: the server
now points at terminals, lsp, exec (for the rooms' execute endpoint) and
documents as well as api and gateway. `ganymede-documents` depends only
on nbformat and `yrs` and does no I/O (a room is a state machine the host
feeds), so it sits in the No I/O band with protocol and nbformat. The
SVG keeps the old file's styles, so `themed.ts` writes light and dark
copies as before; the generator script stays out of the repo, as for the
other diagrams.
