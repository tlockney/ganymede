## 2026-11-25, Marker for ganymede-ui's new browser header (#949)

ganymede-ui#801 replaces the browser toolbar with a status line under the
title, Run Cell with a run menu, Interrupt, Share and a ⋯ menu. The
Toolbar line in `docs/app.md` still describes the released app, so it gets
an `**Unreleased in ganymede-ui.**` note describing `main`'s toolbar;
ganymede-ui's release script opens the issue to update it. An HTML-comment
marker fails `check-unreleased` in Markdown (only `site/index.html` allows
one), so it is a visible note.
