## 2026-11-27, the book's first open in the browser (#953)

ganymede-ui#815 replaced the connect sheet on a browser's first open
with a start screen: Try a Notebook, Open a Notebook… and Connect to a
Server…. It shipped in ganymede-ui 0.17.0 (assets in 0.17.1). #954 put
a marker in docs/app.md and #974 rewrote that page once the release
was out; chapter 11 of the book (site/book/index.html, "Which
backend") still ended at the connect sheet. It now ends at the start
screen, with Connect to a Server… opening the sheet. No Unreleased
note: the change is in ganymede-ui's latest release.

The line also says that a saved connection that fails opens the connect
sheet with its error (`App.tsx`'s connect `.catch`). The book is
generated from the code architecture primer: its chapter 11 "Which
backend" line needs the same change before the next rebuild, or the
rebuild brings back the old text.
