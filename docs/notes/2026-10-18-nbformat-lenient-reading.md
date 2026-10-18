## 2026-10-18, nbformat reading as lenient as Python's (#73)

`nbformat.read` only logs validation errors, but our typed model rejected
whole notebooks over one bad field: an unknown `output_type`, a stream
without `name`, `execution_count: 1.0` or `-1`, `source: null`, a lone
surrogate escape. Such notebooks were "Unreadable Notebook" on GET and
"Invalid notebook" on PUT. Missing and duplicate cell ids were never
repaired either.

- An output that does not fit the schema is kept as `Output::Unknown(Value)`
  and written back unchanged (fixture `invalid_outputs.ipynb` round-trips
  byte for byte against `nbformat.write`).
- Missing `name` reads as `"stdout"` (`new_output`'s default), missing
  `ename`/`evalue` as `""`; `null` metadata, source, outputs and data read
  as empty.
- `execution_count`: an integral float reads as that integer, anything
  else invalid (`-1`, `1.5`, strings) as `None`, so it is saved as `null`.
  Keeping `-1` would need a non-integer type for the field.
- Lone surrogates read as U+FFFD (only after a failed parse, so valid
  files pay nothing). Python keeps them but then cannot write the file as
  UTF-8.
- For nbformat 4.5+, ids are filled (8 hex digits) and duplicates
  replaced on read and on write, as `validator._normalize` does. Earlier
  minors are untouched.

Not done: jupyter_server's `message` field with the validation text. That
needs a schema validator to report what was wrong; the reader now repairs
instead of failing, so there is nothing to put in it yet.
