// Tests for the cell API fetch's link rewriting (#607): `npm test` in docs/.
import { test } from "node:test";
import assert from "node:assert/strict";
import { relink, sources } from "./fetch-cell-api.mjs";

const [cdp, device] = sources;

test("links between the pages follow the renames", () => {
  assert.equal(relink("[w](widgets-reference.md#comm)", cdp), "[w](widgets.md#comm)");
  assert.equal(relink("[j](jupyter-reference.md)", cdp), "[j](jupyter.md)");
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
