## 2026-10-19, reading nbformat 3 notebooks (#142)

`ganymede-nbformat` now reads nbformat 3 files by upgrading them to 4.5, as `nbformat.reads(s, as_version=4)` does: the v3 reader's `strip_transient` and `rejoin_lines`, then `v4.convert.upgrade`. Worksheets are flattened, `heading` cells become markdown (`"#" * level + " "` plus the source's lines joined with spaces), `html` cells become markdown, code cells move `input`, `prompt_number` and `collapsed`, `pyout`/`pyerr` become `execute_result`/`error`, and output keys such as `png` become MIME types. A JSON string under `json` is parsed. Each cell gets a fresh id. nbformat 1 and 2 are still rejected.

`orig_nbformat: 3` stays in the read model's metadata, as in Python, so a contents GET shows it as jupyter_server's does; the writer's `strip_transient` drops it on save. The v4 read path still strips it, as before.

Test: `tests/fixtures/v3.ipynb` and `v3.expected.ipynb` come from `generate.py`; the Rust read of the first, with ids replaced, writes the second byte for byte.

Lesson: v3 joins split lines two ways. Lists from `splitlines(True)` keep their endings and are concatenated; older files used plain `splitlines()` and need `"\n"` between items. nbformat picks by whether the first line ends in a newline, and so does the upgrade.
