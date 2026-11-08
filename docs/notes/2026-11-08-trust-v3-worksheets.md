## 2026-11-08, Trust: nbformat 3 notebooks are checked by their worksheets (#531)

`trust::check_cells` looked only at the top-level `cells`. An nbformat 3
notebook keeps its cells under `worksheets[*].cells`, so nothing was
checked, the notebook counted as trusted, and a save signed it whatever its
outputs held. nbformat's `NotebookNotary` walks the worksheets for v3, uses
v3's names for the unsafe outputs (`pyout` and `display_data`) and safe
keys (`prompt_number`, not `execution_count`), and never trusts a notebook
older than v3. `check_cells` and `mark_cells` now do the same.

The fixture generator (`tests/fixtures/generate_trust.py`) gained five v3
and v2 cases, recorded from nbformat 5.11.1; the existing fixtures came out
unchanged.
