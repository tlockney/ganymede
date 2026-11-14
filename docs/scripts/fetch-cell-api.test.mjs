// Tests for the cell API fetch's link rewriting (#607): `npm test` in docs/.
import { test } from "node:test";
import assert from "node:assert/strict";
import { relink, sources } from "./fetch-cell-api.mjs";

const [cdp, device] = sources;

test("links between the pages follow the renames", () => {
  assert.equal(relink("[w](widgets-reference.md#comm)", cdp), "[w](widgets.md#comm)");
  assert.equal(relink("[j](jupyter-reference.md)", cdp), "[j](jupyter.md)");
  assert.equal(relink("[r](reference.md)", device), "[r](device.md)");
});

test("cdp-kernel's guides go to cdp-kernel.org", () => {
  assert.equal(relink("[g](jupyter-api.md#html)", cdp), "[g](https://cdp-kernel.org/docs/jupyter-api#html)");
});

test("this site's pages become relative, other links go to GitHub", () => {
  assert.equal(relink("[d](https://ganymede-server.com/docs/ganymede-ui/device#in-a-browser)", device), "[d](../ganymede-ui/device.md#in-a-browser)");
  assert.equal(relink("[p](protocol.md)", device), "[p](https://github.com/tlockney/ganymede-ui/blob/main/packages/device/docs/protocol.md)");
  assert.equal(relink("[p](plan.md#phases)", device), "[p](https://github.com/tlockney/ganymede-ui/blob/main/packages/device/docs/plan.md#phases)");
});

test("anchors, absolute URLs and images stay", () => {
  for (const md of ["[a](#x)", "[b](https://example.com/a.md)", "![c](images/widgets/dice.png)"]) assert.equal(relink(md, cdp), md);
});

test("fenced code is left alone", () => {
  const md = "[g](widgets.md)\n\n```ts\nhandlers[name](msg);\n```\n\n  ````md\n  [x](widgets.md)\n  ````\n";
  assert.equal(
    relink(md, cdp),
    "[g](https://cdp-kernel.org/docs/widgets)\n\n```ts\nhandlers[name](msg);\n```\n\n  ````md\n  [x](widgets.md)\n  ````\n",
  );
});

test("a page's cell blocks get the outputs its notebook saved", async () => {
  const { withOutputs, notebookOf } = await import("./fetch-cell-api.mjs");
  const page = "Intro.\n\n<!-- notebook: public/notebooks/p.ipynb -->\n\n```ts cell\n1 + 1\n```\n\n```ts\nnot a cell\n```\n";
  assert.equal(notebookOf(page), "public/notebooks/p.ipynb");
  const nb = {
    metadata: {},
    cells: [{ cell_type: "code", source: ["1 + 1  \n"], outputs: [{ output_type: "execute_result", data: { "text/plain": "\x1b[33m2\x1b[39m" } }] }],
  };
  const md = withOutputs(page, nb, "p", new Map());
  assert.ok(!md.includes("notebook:"));
  assert.ok(md.includes("```ts\n1 + 1\n```\n\n```text\n2\n```"), md);
  assert.ok(md.includes("```ts\nnot a cell\n```"));
  assert.throws(() => withOutputs(page, { metadata: {}, cells: [] }, "p", new Map()), /isn't in its notebook/);
});

test("an anywidget saved with its state runs on the page, as of its own cell", async () => {
  const { output } = await import("./fetch-examples.mjs");
  const saved = { "w-1": { model_module: "anywidget", state: { _esm: "export default {}", count: 3, later: true }, buffers: [{ path: ["b"], data: "AA==", encoding: "base64" }] } };
  const o = { output_type: "display_data", data: { "application/vnd.jupyter.widget-view+json": { model_id: "w-1" } }, metadata: { docs: { state: { count: 1 }, buffers: [] } } };
  const html = output(o, "p", new Map(), saved);
  const model = JSON.parse(html.match(/data-model="([^"]*)"/)[1].replace(/&quot;/g, '"').replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&amp;/g, "&"));
  assert.deepEqual(model.state, { _esm: "export default {}", count: 1 });
  assert.deepEqual(model.buffers, []);
  // Without saved state, or without a snapshot (the example pages'
  // notebooks), there's nothing to start from, so the note stays.
  assert.match(output(o, "p", new Map(), {}), /needs a running kernel/);
  assert.match(output({ ...o, metadata: {} }, "p", new Map(), saved), /needs a running kernel/);
});

test("a longer closing fence ends the block, and identical blocks get their own cells", async () => {
  const { withOutputs } = await import("./fetch-cell-api.mjs");
  const page = "```ts cell\nx\n````\n\n```ts cell\nx\n```\n";
  const result = (t) => ({ output_type: "execute_result", data: { "text/plain": t } });
  const nb = { metadata: {}, cells: ["first", "second"].map((t) => ({ cell_type: "code", source: "x", outputs: [result(t)] })) };
  const md = withOutputs(page, nb, "p", new Map());
  assert.ok(md.indexOf("first") < md.indexOf("second") && md.includes("second"), md);
});

test("prose right after a cell block stays apart from its output; indented blocks aren't cells", async () => {
  const { withOutputs } = await import("./fetch-cell-api.mjs");
  const nb = { metadata: {}, cells: [{ cell_type: "code", source: "x", outputs: [{ output_type: "execute_result", data: { "text/html": "<b>hi</b>" } }] }] };
  const md = withOutputs("```ts cell\nx\n```\nAfter **this**.\n\n  ```ts cell\n  y\n  ```\n", nb, "p", new Map());
  assert.match(md, /<\/div>\n\nAfter \*\*this\*\*\./);
  assert.ok(md.includes("  ```ts cell\n  y\n  ```"));
});
