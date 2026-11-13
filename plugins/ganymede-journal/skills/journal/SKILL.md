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

1. Before a new journal, check what earlier work found:
   `notebooks_search(query)` with the topic's key words (the data set,
   the question), across the whole project, not only `journal/`: the
   user's own notebooks count as much as earlier journals. Read the hits
   that bear on the question and say in the goal cell what you build on.
   Skip this if the tool isn't there (Ganymede 0.7.0 and older).
2. If the user named a notebook, or the work already lives in one of
   their notebooks (see "Journal and notebook" below), work in that.
   Otherwise make one under `journal/`, named
   `YYYY-MM-DD-<topic>.ipynb`, with `notebook_open(path, title)`. The
   title is the question. Choose the kernel in this order:
   - the one the user asks for;
   - a folder default: call `kernelspecs_list(dir: <the notebook's
     folder>)`, such as `dir: "journal"`; when it shows `folder default
     ... (from <file>)`, leave `kernel` out and the server uses it. (The
     session-start note covers only the session's own folder.) Someone
     chose it; don't second-guess it. (Folder defaults need a Ganymede
     newer than 0.10.0; an older one shows none, so infer);
   - otherwise infer it: a preference the project states (CLAUDE.md,
     AGENTS.md); else the kernel of the user's notebooks on the topic
     or in the same folder; else the project's language
     (`pyproject.toml` or `requirements.txt` for Python, `package.json`
     or `deno.json` for TypeScript);
   - else `cdp-node` (TypeScript). Its `npm:` imports are fetched when
     the cell runs, so the notebook names its own libraries and re-runs
     where cdp-kernel is installed; a Python kernel only has what its
     environment has.
   Say in the goal cell which kernel and why when you inferred it. Call
   `kernelspecs_list` to see what is installed. If the user names a
   kernel for a folder as a standing preference, offer to save it as
   `kernel = "<name>"` in that folder's `.ganymede.toml`.
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
    A tool reply that ends with an `@@changed` line means someone
    changed the notebook since your last read: read it in with the call
    the line gives before your next step. An edit to a cell they
    changed is refused until you have; then redo it on top of their
    text, keeping what they wrote.

## Journal and notebook

A user may move between asking you (the journal) and working by hand in
their own notebooks, in JupyterLab, VS Code or the Ganymede apps, for
the same analysis. Someone deep in the Jupyter ecosystem may do most of
the work there. Keep both sides usable:

- Work where the work is. If the user's notebook on the topic is the
  one they keep, add to it rather than starting a parallel journal; ask
  when it isn't clear. Cells you add carry provenance, theirs don't, so
  both stay readable in one file.
- Match their setup: their kernel, their libraries, their data paths.
  Don't move a Python user to TypeScript, or the reverse.
- Share code through a module both import (a `.py` or `.ts` file next
  to the notebooks), not by copying cells between notebooks. Changes to
  a module need a kernel restart (or `importlib.reload` in Python) in
  every notebook that imported it.
- Keep the journal runnable in plain Jupyter: no step should depend on
  this plugin or on Ganymede to re-run. `notebook_run_all` checks the
  order; a user re-running it in JupyterLab gets the same results.
- When the user worked by hand since your last turn, read what changed
  in their notebook (`notebook_read`, with `since` if you have a mark)
  before you build on it, and say how you used it.
- When a result belongs in the user's notebook, offer to add it there
  (the code and a short note) rather than leaving it only in the
  journal. Tell the user which notebook you wrote to, by path.

## Output, charts and widgets

14. Format results with the kernel's display module rather than
    printing them: the `jupyter` global in TypeScript (`jupyter.md`,
    `jupyter.svg`, `jupyter.display`), `IPython.display` in Python
    (`display`, `Markdown`, `SVG`, `Image`). A Markdown table of the
    numbers that matter, under a short heading, reads better than a
    printed data frame, and you can read it back. To show progress or
    a value that changes, display once with a display id and update it
    in place instead of printing lines. See
    [typescript.md](typescript.md) and [python.md](python.md).
15. When a result needs a chart, pull in a plotting library and pin
    its version: Observable Plot in TypeScript (an `npm:` import),
    matplotlib in Python. A Vega-Lite spec suits a chart only the
    person needs to read (see 16). A Python package the kernel lacks
    goes in with `%pip install name==version` in a cell of its own, so
    the notebook says what it needs; a TypeScript cell's import is
    enough. Prefer TypeScript for a journal that needs libraries the
    environment may not have.
16. Look at the chart before writing the finding: a chart can show
    what the table you checked doesn't, such as a trend at the end of a
    series. When the reply to `cell_run` has no image (the server
    doesn't send them), call `cell_snapshot(ids: [<the cell id>], code:
    false)`, which always returns the image. You can see SVG and PNG
    charts; charts drawn by JavaScript in the frontend (Vega-Lite,
    Plotly, Altair) reach you only as text, so draw one you need to
    read as SVG or PNG. `cell_snapshot` needs a Ganymede newer than
    0.7.0; without it, say what the chart is meant to show and check
    the numbers in a table instead.
17. When a person is watching and would explore a result, such as
    moving a threshold, picking a date range or a category, add a
    widget wired to the kernel: `jupyter.widget` in TypeScript,
    ipywidgets in Python. Widgets show in the Ganymede apps,
    JupyterLab and VS Code. You can't see or move one, and a published
    page shows only its plain text, so every finding still rests on a static output
    (a chart or table) for the value you write about.

## Show the user

18. When you tell the user a finding, show the evidence in your reply,
    not only the notebook path: call `cell_snapshot` on the cells behind
    it and paste the Markdown it returns. It gives the chart as an image
    file linked by path, a table as a Markdown table, and the code as a
    fenced block. Pick the one or two cells that carry the point; leave
    out the code (`code: false`) unless the user asked how. For a
    report next to the notebook, pass `dir: "journal/snapshots"` so
    the images are saved under the project.

## Finish

19. Run `notebook_run_all`. It restarts the kernel and runs every cell;
    fix anything that fails, or say plainly why it can't reproduce.
20. Add a Markdown summary after the title (`cell_add(after: <title
    cell id>)`): the findings as a short numbered list, and what is
    still open.
21. For a longer journal, ask the `journal-reviewer` agent to read it
    cold, and fix the gaps it finds.

## Publish

22. When the user asks to publish or share the journal, finish it first
    (19-21), then call `notebook_publish`. It saves one self-contained
    HTML page, `published/<name>.html` beside the notebook, and returns
    its path on disk. The page holds the notebook's cells and outputs
    only, with the code folded under each result; pass `code: "hidden"`
    for readers who don't want code, or `"shown"` for ones who do.
23. If you have a tool that hosts a page, such as an artifact tool,
    publish that file with it as it is, and give the user the link.
    The page has a Download notebook button; on a claude.ai artifact
    it needs the `downloads` capability, so declare
    `capabilities: {downloads: true}` when you publish it there.
    Otherwise give the path. Don't rewrite the page by hand: a fix
    belongs in the notebook, then publish again.
24. Before you host it anywhere, read the notebook once for what
    shouldn't leave the machine. The server replaces its root path, the
    home directory and its token (if 8 characters or more), but not
    other hosts, names or values
    a cell printed: clear those outputs (`cell_clear_outputs`) and
    publish again. `notebook_publish` needs a Ganymede newer than
    0.8.0 (`ganymede nb publish <notebook>` makes the same page from
    the saved file); without either, tell the user it isn't available.
