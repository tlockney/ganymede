# Notebooks as programs

A notebook can take parameters and hand back named results, so it runs
like a function: from the command line, from CI, over HTTP, from a link
or a Shortcut on a Mac, and from another notebook. Every one of those
callers uses the same run: a fresh kernel from the notebook's own
kernelspec, the parameters as JSON, and the named results back as JSON.

This page follows one notebook through each caller. The formats are in
[Parameters and named results](reference.md#parameters-and-named-results).

## The notebook

`fremont.ipynb` counts bikes on a Seattle bridge. Its first cell reads
two parameters, with defaults so the notebook still runs on its own; a
later cell publishes one result by name. In TypeScript on
[cdp-kernel](https://cdp-kernel.org) (0.10.0 or later):

```ts
const { counter = "Fremont", days = 30 } = jupyter.params as { counter?: string; days?: number };
```

```ts
// ... the analysis ...
jupyter.result("weekday_drop", -0.19);
```

`jupyter.params` reads the `GANYMEDE_PARAMS` variable the run sets, and
`jupyter.result` writes an output with the MIME type
`application/vnd.ganymede.result+json`. Neither is specific to
TypeScript: a Python notebook reads the variable and publishes the same
MIME type, or uses scrapbook's `glue`, which Ganymede reads too
([Getting started](getting-started.md#parameters-and-named-results) has
the Python version). The server knows no language; it passes JSON in and
reads JSON out.

A run writes nothing back into the notebook unless you ask it to, so the
file on disk keeps its defaults.

## From the command line

```sh
ganymede execute fremont.ipynb -p counter="2nd Ave" -p days=7 --results
# {"path":"fremont.ipynb","status":"ok","results":{"weekday_drop":-0.19}}
```

`-p NAME=VALUE` is repeatable, and a value that parses as JSON is JSON.
`--params-file` takes a JSON object. `--results` prints one JSON line per
notebook, so a script can pipe it to `jq`. Add `-o out.ipynb` or
`--inplace` to keep the executed notebook.

## In CI

A notebook that fails fails the job, so a folder of notebooks can be a
test suite or a scheduled report. The workflow in
[Getting started](getting-started.md#execute-notebooks) runs every
notebook in a folder on each push; add `-p` for a scheduled run with
today's date, and `--results` to hand the numbers to the next step.

## Over HTTP

A running server takes the same run as a request, with its token:

```http
POST /api/notebooks/run
{"path": "fremont.ipynb", "params": {"counter": "2nd Ave", "days": 7}}
```

The answer is the `--results` line: `{ path, status, results, error? }`.
The notebook is read from its open document when someone has it open,
so unsaved edits run, and nothing is written.
[`POST /api/notebooks/run`](reference.md#parameters-and-named-results)
has the timeout and the errors.

## From a link or a Shortcut

The Mac app opens `ganymede://` links and gives Shortcuts a Run Notebook
with Parameters action. Both use the endpoint above in the app's own
server:

```text
ganymede://run?path=~/notebooks/fremont.ipynb&p.counter=2nd%20Ave&p.days=7
```

The first time a link runs a notebook, the app asks; "Always allow"
covers that notebook and that caller only. With an
[x-callback-url](https://x-callback-url.com) reply, the named results go
back to the caller as JSON, so a Shortcut can run the notebook and use
`weekday_drop` in its next action. The app guide has the
[links](ganymede-ui/apps.md#links) and the
[Shortcuts actions](ganymede-ui/apps.md#shortcuts).

## From another notebook

A notebook can run another one through the server's
[host API](host-api.md), if it has been granted that notebook:

```ts
const host = await jupyter.host();
const run = await host!.notebooks.run({ path: "fremont.ipynb", params: { counter: "2nd Ave" } });
run.results.weekday_drop;
```

`host.notebooks.results` reads the results a notebook saved without
running it, and `list`, `read` and `search` find notebooks to run. The
[`jupyter` reference](cell-api/jupyter.md#jupyter-host-options) has each
call.

## Grants

A kernel holds no token. It asks the server, and the server checks what
the notebook it runs for was granted. Out of the box a notebook may read
notebooks under the root and edit its own cells, and may run none:

```sh
ganymede grant journal/index.ipynb run=reports   # may run notebooks in reports/
ganymede grant journal/index.ipynb read=journal  # reads only journal/ and itself
ganymede grant --list
ganymede grant --revoke journal/index.ipynb run=reports
```

A notebook's own grant replaces the defaults key by key, so it can add
access as well as remove it. A run never widens what its caller holds: a
notebook run from another gets the intersection of both grants, so it
can't reach more than its caller. Runs nest at most 3 deep. The apps set the same grants under Kernel ›
Notebook Access…. [The host API](reference.md#the-host-api) has the config
keys, the HTTP form, and how a grant follows a renamed notebook.

What comes back from another notebook is data written by whoever ran
it, possibly an agent reading a web page. Treat results as untrusted
input: they are JSON, never HTML, and the host API doesn't mark them
trusted.

## What's next

Saving each run as a copy
([#658](https://github.com/tlockney/ganymede/issues/658)) and granting
from `ganymede execute`
([#647](https://github.com/tlockney/ganymede/issues/647)) are on the
[Roadmap](roadmap.md).
