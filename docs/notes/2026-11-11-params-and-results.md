## 2026-11-11, parameters and named results for `ganymede execute` (#650)

The first step of the run contract from the direction review: `-p` /
`--params-file` in, `--results` out. The host stays language-free by
passing parameters as data, one JSON object in `GANYMEDE_PARAMS`, and
reading results as a MIME type, `application/vnd.ganymede.result+json`
(`{name, value}`). Python's scrapbook types are read too, and the last
output with a name wins, as in scrapbook.

- An environment variable over a file: every launcher and kernel can
  use it, and it needs no cleanup. It is capped at 32,767 bytes, the
  Windows limit for a variable; bigger inputs belong in a file whose
  path is a parameter.
- `--results` reads only the cells the run started. A cell after a
  failure, or one skipped by tag, still holds outputs an earlier run
  saved, and those aren't this run's results.
- Values are read as JSON when they parse, as papermill reads numbers,
  so `-p days=7` is a number. A string that looks like JSON needs its
  quotes (`-p 'id="007"'`).
- The reader, `ganymede_nbformat::named_results`, is in the format crate
  so the host API's `notebooks.results` and `notebooks.run` (#647) reuse
  it.
- cdp-kernel's `jupyter.params` / `jupyter.result` (cdp-kernel#456) need
  its next release; the tests here write the MIME type by hand, so they
  pass with any release.
