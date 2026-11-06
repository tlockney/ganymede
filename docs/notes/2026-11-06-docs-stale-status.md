## 2026-11-06, stale "not built" statements in the docs (#402)

A docs review against `main` after 0.6.0 found pages still calling shipped
features missing: autorestart, terminals, language servers, file watching
and rooms in `protocol.md`; terminals, LSP and collaboration under
"What is missing" in `switching.md`; reload-on-disk and distribution as
left to do in `ipados.md`; and the same in the `ganymede-api`,
`ganymede-server` and `ganymede-contents` READMEs. Most were written as
"Not yet" lines next to a requirement and never revisited when the
feature landed. Status written beside a design note goes stale; the
review's plan proposes keeping status only in Roadmap and Known gaps.
The server README's flag list is now a link to the reference page
instead of a second copy.
