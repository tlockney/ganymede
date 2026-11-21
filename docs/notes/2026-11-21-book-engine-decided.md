## 2026-11-21, The book says the in-app engine is decided (#922)

The book (`site/book/index.html`) still called a native in-app V8 engine
the open question in chapter 12 and listed it under "Open edges" in
chapter 14, after the roadmap and `docs/in-app-kernel.md` had moved it to
decided (#921, ganymede-ui#765). Chapter 12 now says the engine was tried
and set aside, with the #646 device numbers, and that the spike stays in
`spikes/v8-engine`; the chapter 14 bullet is gone, since it is no longer
an edge where the code moves.

The book has no generator: `site/book/index.html` is its source, so a
change to a decision recorded in `docs/` needs a grep of the book too.
