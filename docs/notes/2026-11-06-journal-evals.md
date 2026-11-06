## 2026-11-06, journaling evals (#316)

`evals/journal/` holds three tasks with known answers, a runner that
runs each through `claude -p` with the ganymede-journal plugin, and
scoring that reads only the finished `.ipynb`. The task data comes from a
seeded generator, so answers don't drift between runs.

Each task carries a trap (duplicated rows, a shell step, a unit mix-up),
so a journal that revises a finding in place has something to revise.
Appended corrections are found by wording, which misses some; revisions
in place are exact, from the cell's `history`.

"How often a person rewrote the summary" needs a person, so the runner
saves each notebook twice: one copy left as the session ended, one to
edit. `--rescore` compares the two.

Baseline: to be run by hand (Thomas, on #316) and recorded here.
