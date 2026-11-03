---
name: journal
description: Keep a live Jupyter notebook as the record of a data exploration or analysis while doing it, using the ganymede notebook tools. Use when the user asks to explore, analyze, chart or investigate data, or says "journal" or "keep a notebook".
---

# Journal an analysis into a notebook

The notebook is the record of the work, kept while you work, not a
write-up at the end. Run analysis code only through the `ganymede`
tools, on the notebook's kernel, so every result in the notebook came
from the code above it. Never write `.ipynb` JSON with Read, Write or
Edit.

## Start

1. If the user named a notebook, open it. Otherwise make one under
   `journal/`, named `YYYY-MM-DD-<topic>.ipynb`, with
   `notebook_open(path, title, kernel)`. The title is the question.
   Default kernel: `cdp-node` (TypeScript); call `kernelspecs_list` if
   it isn't there, and use what the user asks for.
2. Under the title, add a Markdown cell: the goal in a sentence or two,
   the date, who is working, and the kernel.
3. Later tools may leave out `path`: they use the notebook last opened.

## Each step

4. One question per section. Before code, a Markdown cell (`cell_add`)
   says what you are about to find out and why.
5. Run the code that produces a result with `cell_run`, so the notebook
   keeps the code and its output. Keep cells small enough to re-run.
   Code several cells share goes in a module next to the notebook (a
   `.ts` file the kernel imports), written with your Edit tool.
6. Probe with `cell_run(record: false)`: peeking at a value, checking a
   column name, trying a call. Probes run on the same kernel and leave
   nothing in the notebook.
7. After a result, a short Markdown finding: what it shows, in a
   sentence or two, with the number that matters.

## Steps outside the kernel

8. A shell command or a file you wrote is part of the story but didn't
   run in the kernel. Add it with `cell_add(mode: "recorded")`, never as
   kernel output. With this plugin's hooks, a Bash command that ends in
   the comment `# journal` is recorded for you; the hook says so, so
   don't add it twice. Keep credentials out of recorded cells.

## Revise, don't append

9. When a finding turns out wrong, fix it where it stands with
   `cell_edit` and re-run what depends on it (`cell_edit(run: true)`).
   Don't add a correction at the bottom. The replaced text is kept in
   the cell's metadata.

## Working with a person

10. A person may watch the notebook in ganymede-ui or JupyterLab and
    add or change cells. Cells without `metadata.ganymede.agent` were
    written by a person: read them as input, and say in the next note
    how you used them.
11. `notebook_read` ends with an `@@read <mark>` line. Before building
    on the notebook after a pause, call `notebook_read(since: <mark>)`
    to see what others changed. With this plugin's hooks, those changes
    arrive as context when the user writes; work from what is there.

## Charts

12. Draw charts with Observable Plot or Vega-Lite, and read the image
    that comes back before writing the finding. See
    [typescript.md](typescript.md) for cdp-kernel's display API, Polars
    and Plot. An image reaches you only when the server sends images
    (`ganymede mcp --images`, or `mcp_images = true` on a shared
    server); without it, say what the chart is meant to show and check
    the numbers in a table instead.

## Finish

13. Run `notebook_run_all`. It restarts the kernel and runs every cell;
    fix anything that fails, or say plainly why it can't reproduce.
14. Add a Markdown summary after the title (`cell_add(after: <title
    cell id>)`): the findings as a short numbered list, and what is
    still open.
15. For a longer journal, ask the `journal-reviewer` agent to read it
    cold, and fix the gaps it finds.
