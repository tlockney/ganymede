## 2026-11-13, syntax highlighting in published pages (#739)

Thomas noticed that the folded code under each result in a page from
`notebook_publish` had no syntax highlighting. Nothing in the page did:
code cells and fenced Markdown blocks went out as escaped text.

The highlighter is a small lexer in `ganymede-nbformat` (`highlight.rs`)
rather than a grammar crate such as syntect or tree-sitter, which would
be a large dependency for four token kinds. It knows TypeScript and
JavaScript, Python, shell and JSON, the languages journals use (kernels
plus recorded shell steps); anything else stays plain. It marks
keywords, strings, comments, numbers and literals as `hl-*` spans,
coloured by the page's CSS in light and dark. It runs on the server, so
the page still has no script beyond the frame sizer.

Code cells use the notebook's language (`language_info.name`, or the
kernelspec's); Markdown fences use their info string, and keep it as
`class="language-…"` on `<code>` as pulldown-cmark did. Rules it keeps:

- Every character of the source comes out, in order; the tests strip
  the tags and compare.
- A string (backticks too) ends at the line's end when unclosed, so a
  JavaScript regex such as `/'/` can't colour the rest of the cell.
- A word after `.` is a name (`row.type`), not a keyword; in shell, after `/` too.
- In shell, `#` starts a comment only at the start of a word.
