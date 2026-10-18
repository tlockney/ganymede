# ganymede-nbformat

Jupyter notebook file format (nbformat v4, minor 5) for Ganymede:
types, a lenient reader, and a writer whose output is identical to Python
`nbformat`'s. No Python, no I/O beyond `std::fs` in the `read_file` /
`write_file` helpers.

## What it does

- `Notebook`, `Cell` (`Code` / `Markdown` / `Raw`, tagged by `cell_type`),
  `Output` (`Stream` / `DisplayData` / `ExecuteResult` / `Error`, tagged by
  `output_type`), `MimeBundle`, `MultilineString`. Every struct has an
  `extra: serde_json::Map` captured with `#[serde(flatten)]`, so fields this
  crate does not model survive a round trip.
- Reading: `Notebook::from_str` / `from_slice` / `from_reader` / `from_value`
  and `read_file`. As lenient as Python's `nbformat.read`: an output or
  cell that doesn't fit the schema is kept verbatim (`Output::Unknown`,
  `Cell::Unknown`) and written back unchanged, bad fields read as their
  defaults, and `from_slice_with_report` / `from_value_with_report` also
  return what was wrong, worded like `jsonschema`. `source`, stream `text` and text-like MIME values may be a
  string or an array of strings (joined on read, as nbformat's `rejoin_lines`
  does). Transient metadata (`orig_nbformat`, `orig_nbformat_minor`,
  `signature`, cell `trusted`) is stripped, as nbformat's `strip_transient`
  does on both read and write. Only `nbformat == 4` is accepted; other
  versions fail with `Error::UnsupportedVersion` (v3 upgrade is a TODO).
- Writing: `Notebook::to_string()` (via `Display`) equals Python
  `nbformat.writes(nb)`; `Notebook::to_file_string()`, `to_writer` and
  `write_file` equal what `nbformat.write(nb, path)` puts in the file, which is
  the same text plus a trailing newline. `to_disk_value()` exposes the prepared
  JSON tree.

## Writer fidelity rule

A notebook read by this crate and written back must be byte-for-byte what
Python nbformat 5.x writes for the same notebook, so that a save from this
server never produces a git diff against a save from Jupyter. The writer
reproduces, in order, what `nbformat.v4.nbjson.JSONWriter.writes` and
`nbformat.write` do:

1. `split_lines`: cell `source` and stream `text` strings become arrays of
   lines via Python's `str.splitlines(keepends=True)`; in `display_data` /
   `execute_result` `data` and in cell `attachments`, string values under
   `text/*`, `application/javascript` and `image/svg+xml` are split the same
   way. JSON MIME types (`application/json`, `application/*+json`) and every
   other MIME type are left alone. `splitlines` breaks on `\n`, `\r`, `\r\n`,
   `\v`, `\f`, `\x1c`, `\x1d`, `\x1e`, `\x85`, ` ` and ` `, the
   exact set CPython uses; an empty string becomes `[]`.
2. `strip_transient` (see above).
3. `json.dumps(indent=1, sort_keys=True, separators=(",", ": "),
   ensure_ascii=False)`: keys sorted by code point at every level, one-space
   indentation, `[]` / `{}` for empty containers, non-ASCII written raw,
   control characters escaped as Python does. Floats are laid out like
   Python's `float.__repr__` (`1e-05`, `1e+16`), not like serde_json's
   default (`0.00001`, `1e16`).
4. A trailing newline, added by `nbformat.write`.

Numbers keep their source text (serde_json's `arbitrary_precision`), so
integers of any size round-trip exactly, as Python's `int` does, and
floats parse correctly rounded. For 4.5+ notebooks, missing or duplicate
cell ids are filled in on read and on write, as Python's `validate()`
does.

Known gaps, outside what JSON can carry: `NaN` / `Infinity` literals,
which Python's `json` accepts and emits, are rejected as invalid JSON, and
a float too large for `f64` keeps its text where Python writes
`Infinity`.

## Tests and fixtures

`tests/fixtures/*.ipynb` are produced by the reference implementation.
`tests/roundtrip.rs` reads each `nbformat.write`-generated fixture, writes it
back and asserts the bytes are identical, and reads `unsplit_source.ipynb`
(sources as single strings, unsorted keys, indent 2, transient metadata
present) and asserts the output equals `unsplit_source.expected.ipynb`, which
is what nbformat writes after reading the same file. Unit tests cover
`splitlines`, `MultilineString` and `MimeBundle` serde, key sorting, float
layout and version rejection.

To regenerate the fixtures, with a Python environment that has `nbformat`
installed:

```sh
python tests/fixtures/generate.py
```

Then run:

```sh
cargo test -p ganymede-nbformat
cargo clippy -p ganymede-nbformat --all-targets
```
