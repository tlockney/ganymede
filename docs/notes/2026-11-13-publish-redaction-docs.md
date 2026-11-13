## 2026-11-13, what `nb publish` redacts (#769)

The security read of 2026-11-13 (#746, finding 6) noted that the publish
docs list the root, home and token replacements without saying they are
the only ones. `publish_redactions` (`routes.rs`) builds exactly those
three; anything else a cell printed, such as an API key or an
environment variable, goes into the page and its notebook copy as it
is. `docs/mcp.md` "Publishing a notebook" and the `nb publish` flags in
`docs/reference.md` now say so and ask the reader to check the page
before sharing it. No code changed: a secret scanner would guess, and a
page that looks scrubbed but isn't is worse than one that says it isn't.
