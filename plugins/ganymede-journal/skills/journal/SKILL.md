---
name: journal
description: Keep a live Jupyter notebook as the record of a data exploration or analysis while doing it, using the ganymede notebook tools. Use when the user asks to explore, analyze, chart or investigate data, says "journal" or "keep a notebook", or asks to publish or share a journal as a page.
---

# Journal an analysis into a notebook

The notebook is the record of the work, kept while you work, not a
write-up at the end. Run analysis code only through the `ganymede`
tools, on the notebook's kernel, so every result in the notebook came
from the code above it. Never write `.ipynb` JSON with Read, Write or
Edit.

## Start

1. Before a new journal, check what earlier ones found:
   `notebooks_search(query, dir: "journal")` with the topic's key words
   (the data set, the question). Read the hits that bear on the question
   and say in the goal cell what you build on. Skip this if the tool
   isn't there (Ganymede 0.7.0 and older).
2. If the user named a notebook, open it. Otherwise make one under
   `journal/`, named `YYYY-MM-DD-<topic>.ipynb`, with
   `notebook_open(path, title, kernel)`. The title is the question.
   Default kernel: `cdp-node` (TypeScript); call `kernelspecs_list` if
   it isn't there, and use what the user asks for.
3. Under the title, add a Markdown cell: the goal in a sentence or two,
   the date, who is working, and the kernel.
4. Later tools may leave out `path`: they use the notebook last opened.

## Each step

5. One question per section. Before code, a Markdown cell (`cell_add`)
   says what you are about to find out and why.
6. Run the code that produces a result with `cell_run`, so the notebook
   keeps the code and its output. Keep cells small enough to re-run.
   Code several cells share goes in a module next to the notebook (a
   `.ts` file the kernel imports), written with your Edit tool.
7. Probe with `cell_run(record: false)`: peeking at a value, checking a
   column name, trying a call. Probes run on the same kernel and leave
   nothing in the notebook, but the names they declare stay defined. In
   TypeScript, wrap a probe in a block, `{ const { hourly } = await
   import("./data.ts"); hourly().head(); }`, so a later cell's
   `import { hourly }` or `let` doesn't fail with "already been declared".
8. Keep outputs. They are the record of what the run produced, and the
   server signs the notebook so charts and HTML render when someone
   opens it. Leave a cell's outputs out only when they don't belong in
   the file: too large (a full table dump, thousands of log lines) or
   private (personal data, internal hosts). Pass
   `cell_run(keep_outputs: false)`, or call `cell_clear_outputs(id)`
   after the fact; the code stays, and later runs leave the outputs out
   too. These need a Ganymede newer than 0.7.0: if `cell_clear_outputs`
   isn't in your tools, the server ignores `keep_outputs`, so use
   `record: false` for such output instead. Say in the next note what the output showed. Never print
   credentials, even with outputs left out: they reach the notebook
   while the cell runs.
9. After a result, a short Markdown finding: what it shows, in a
   sentence or two, with the number that matters.

## Steps outside the kernel

10. A shell command or a file you wrote is part of the story but didn't
   run in the kernel. Add it with `cell_add(mode: "recorded")`, never as
   kernel output. With this plugin's hooks, a Bash command that ends in
   the comment `# journal` is recorded for you; the hook says so, so
   don't add it twice. Keep credentials out of recorded cells.

## Revise, don't append

11. When a finding turns out wrong, fix it where it stands with
   `cell_edit` and re-run what depends on it (`cell_edit(run: true)`).
   Don't add a correction at the bottom. The replaced text is kept in
   the cell's metadata.

## Working with a person

12. A person may watch the notebook in ganymede-ui or JupyterLab and
    add or change cells. Cells without `metadata.ganymede.agent` were
    written by a person: read them as input, and say in the next note
    how you used them.
13. `notebook_read` ends with an `@@read <mark>` line. Before building
    on the notebook after a pause, call `notebook_read(since: <mark>)`
    to see what others changed. With this plugin's hooks, those changes
    arrive as context when the user writes; work from what is there.

## Charts

14. Draw charts with Observable Plot or Vega-Lite, and look at the
    chart before writing the finding: a chart can show what the table
    you checked doesn't, such as a trend at the end of a series. When
    the reply to `cell_run` has no image (the server doesn't send them),
    call `cell_snapshot(ids: [<the cell id>], code: false)`, which always
    returns the image. See [typescript.md](typescript.md) for
    cdp-kernel's display API, Polars and Plot. `cell_snapshot` needs a
    Ganymede newer than 0.7.0; without it, say what the chart is meant
    to show and check the numbers in a table instead.

## Show the user

15. When you tell the user a finding, show the evidence in your reply,
    not only the notebook path: call `cell_snapshot` on the cells behind
    it and paste the Markdown it returns. It gives the chart as an image
    file linked by path, a table as a Markdown table, and the code as a
    fenced block. Pick the one or two cells that carry the point; leave
    out the code (`code: false`) unless the user asked how. For a
    report next to the notebook, pass `dir: "journal/snapshots"` so
    the images are saved under the project.

## Finish

16. Run `notebook_run_all`. It restarts the kernel and runs every cell;
    fix anything that fails, or say plainly why it can't reproduce.
17. Add a Markdown summary after the title (`cell_add(after: <title
    cell id>)`): the findings as a short numbered list, and what is
    still open.
18. For a longer journal, ask the `journal-reviewer` agent to read it
    cold, and fix the gaps it finds.

## Publish

19. When the user asks to publish or share the journal, finish it first
    (16-18), then call `notebook_publish`. It saves one self-contained
    HTML page, `published/<name>.html` beside the notebook, and returns
    its path on disk. The page holds the notebook's cells and outputs
    only, with the code folded under each result; pass `code: "hidden"`
    for readers who don't want code, or `"shown"` for ones who do.
20. If you have a tool that hosts a page, such as an artifact tool,
    publish that file with it as it is, and give the user the link.
    Otherwise give the path. Don't rewrite the page by hand: a fix
    belongs in the notebook, then publish again.
21. Before you host it anywhere, read the notebook once for what
    shouldn't leave the machine. The server replaces its root path, the
    home directory and its token (if 8 characters or more), but not
    other hosts, names or values
    a cell printed: clear those outputs (`cell_clear_outputs`) and
    publish again. `notebook_publish` needs a Ganymede newer than
    0.8.0 (`ganymede nb publish <notebook>` makes the same page from
    the saved file); without either, tell the user it isn't available.
