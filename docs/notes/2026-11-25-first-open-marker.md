## 2026-11-25, Marker for the browser's new first open (#953)

ganymede-ui#815 replaces the connect sheet on a first open in the browser
with a start screen: Try a Notebook, Open a Notebook… and Connect to a
Server…. `docs/app.md`'s "With no server" section gets an
`**Unreleased in ganymede-ui.**` note saying so, which ganymede-ui's
release script lists for a follow-up. The book's "Which backend" line
(`site/book/index.html`, chapter 11, "…then the connect sheet") isn't
scanned for markers, so #953 stays open to change it to "then the start
screen" once ganymede-ui releases it.
