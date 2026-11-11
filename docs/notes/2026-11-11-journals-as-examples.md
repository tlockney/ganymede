## 2026-11-11, the two agent journals as examples (#689)

The Fremont Bridge and Oklahoma earthquakes journals, kept with the
ganymede-journal plugin on 2026-11-09, are now in examples/journals/,
each with the module it imports, a `fetch-data.sh` that downloads its
data as the notebook's recorded cells did, and `prompts.md`.
`examples/journals/run.sh` fetches the data and runs each notebook in
place with `ganymede execute --inplace`; data/ is not committed.

Re-run on Ganymede 0.8.0 and cdp-kernel 0.10.0 (cdp-node, Node 22) with
freshly downloaded data, both notebooks ran clean (11 and 30 cells) and
every output matched the saved one, so the findings still read true.
Seattle's export now lists rows in a different order but holds the same
rows. Oklahoma downloads about 55 MB (the OCC injection workbooks) and
runs in about a minute.

The Oklahoma prompts come from the session transcript. The Fremont
session was driven by a script, so its prompts were written afterwards
for the example; prompts.md and the page say so.

`npm run fetch-examples` renders both as `/docs/examples/journal-<name>`,
with the prompts in a box after the title. pages.yml now runs on changes
to examples/journals/, and scripts/ci-changes.sh counts examples/ as not
code.
