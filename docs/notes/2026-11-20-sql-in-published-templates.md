## 2026-11-20, SQL in `jupyter.sql` templates on published pages (#912)

A cdp-kernel notebook runs DuckDB through `jupyter.sql`, as a tag
(`` jupyter.sql`select …` ``) or as `` jupyter.sql.query(`…`) ``. The
published page showed each query as one TypeScript string, so a long
query had no keywords, strings or comments to read by.

`highlight.rs` now lexes the template of a `jupyter.sql` tag, and the
first argument of `jupyter.sql.query(`, as SQL: keywords (case-insensitive,
a DuckDB-flavoured list including common type names) as `hl-k`, `'…'` as
`hl-s`, numbers and `true`/`false`/`null` as `hl-n`, `--` and `/* */` as
`hl-c`. A `"quoted name"` and a qualified name (`t.date`) stay plain. The
backticks keep `hl-s`. Every other template is unchanged, `sql` tags
included: ganymede-ui highlights any `sql` tag (its #708), but this
change follows the issue and matches only `jupyter.sql`.

Each `${…}` is cut out and highlighted by the TypeScript lexer, and the
SQL lexer's state (code, inside a quote, inside a comment) carries across
it, so `'${FROM}'` comes out as `'` (string) `${FROM}` (TypeScript) `'`
(string), and the string closes. Finding a template's end and an
interpolation's end follows braces and skips strings, comments and
nested templates; the plain-template path still uses the old
backtick-to-backtick rule, so non-SQL templates render as before. An
unclosed template or interpolation runs to the end of the code, and every
character still comes out in order.

The lexer branches on the code's language as it already did; the host
crates still know nothing of the kernel's language.

Tests in `highlight.rs`: `sql_tag` and `sql_query_call` (the issue's two
examples, checked span by span: keywords, the `'${FROM}'` pieces, the
`${SENSOR}` interpolation outside any span, the `--` comment ending at
its line), `sql_interpolation_is_typescript`, `other_templates_stay_strings`
(untagged, `sql`, `db.sql`, `myjupyter.sql`, `.run(`, a second argument,
Python) and `sql_templates_keep_every_character`. Checked by hand with
`ganymede nb publish` on a notebook with both examples, in light and dark.
