## 2026-10-18, notebook validation message, unknown cells (#73)

Follow-up to the lenient nbformat reading entry above,
which left three gaps.

- An unknown `cell_type` (nbformat 3's `heading`, or anything newer) made
  the whole notebook unreadable. It is now `Cell::Unknown(Value)`, kept and
  written back unchanged like `Output::Unknown`; so is a known cell whose
  fields do not fit even leniently (`source: 5`). Fixture
  `invalid_cells.ipynb` round-trips byte for byte against `nbformat.write`.
  `Cell::source`, `metadata` and `cell_type` now return `Option`.
- jupyter_server adds `message: "Notebook validation failed: ..."` to the
  model of a notebook that breaks the schema, on GET with content and on
  save. The reader now returns a `ReadReport` (`from_slice_with_report`,
  `from_value_with_report`): a schema check of exactly the spots the
  lenient reader defaults, empties or keeps verbatim, worded like
  `jsonschema` and located like `instance['cells'][1]['outputs'][0]`. The
  contents model carries it as `message`, absent when valid. It is not a
  full schema validator, and the text is not jsonschema's word for word.
  Missing or duplicate cell ids are not reported, since nbformat repairs
  them before validating.
- A lone surrogate in a PUT body failed with "invalid JSON body". The
  server's body parser now retries with lone surrogates read as U+FFFD,
  as the file reader already did.
