## 2026-11-18, the published page's style in its own file, and a skill (#867)

Thomas asked where the published-page styling lives and whether other
pages could use it. It was a 130-line Rust string constant in
`ganymede-nbformat`'s `html.rs`, with the palette also copied by hand
into `site/style.css` and the docs theme.

The constant is now `src/page.css`, embedded with `include_str!`, so
published pages are byte for byte the same. The journal plugin has a
new `page-style` skill that describes the page (rules, palette, type,
structure, class names) for agents writing other pages, such as a
report across journals. A plugin installs only its own folder, so the
skill carries a copy of the stylesheet; a test in `html.rs` fails when
the copy differs and rewrites it with `UPDATE_REFERENCE=1`, the same
pattern as `docs/mcp-tools.md`.

Both copies are `eol=lf` in `.gitattributes`: `include_str!` keeps line
endings, so a CRLF checkout (the Windows release build) would change
the page.

Left as they are: `FRAME_CSS` (the one-line style inside sandboxed
output frames), the site's and docs theme's own copies of the palette,
and custom templates for publishing, which wait for a second real use.
