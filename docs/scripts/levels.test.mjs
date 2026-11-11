// The sidebar's groups (docs/.vitepress/levels.ts): each of ganymede-ui's
// pages shows once, whether levels.ts places it or not.
import { test } from "node:test";
import assert from "node:assert/strict";
import { groups, levelOf, uiUnplaced } from "../.vitepress/levels.ts";

const links = (has) => groups(has).flatMap((g) => g.items.map((p) => p.link));

test("each copied ganymede-ui page shows once", () => {
  const pages = ["apps", "architecture", "deploying", "device", "releasing", "shortcuts", "new-page"];
  const placed = links((name) => pages.includes(name)).filter((l) => l.startsWith("/ganymede-ui/"));
  const extra = uiUnplaced(pages).map((name) => `/ganymede-ui/${name}`);
  const all = [...placed, ...extra].sort();
  assert.deepEqual(all, pages.map((name) => `/ganymede-ui/${name}`).sort());
  assert.deepEqual(uiUnplaced(pages), ["new-page"]);
});

test("pages the copy lacks are left out", () => {
  assert.ok(!links(() => false).some((l) => l.startsWith("/ganymede-ui/")));
});

test("no link appears twice", () => {
  const all = links();
  assert.equal(new Set(all).size, all.length);
});

test("ganymede-ui pages get no level", () => {
  assert.equal(levelOf("ganymede-ui/releasing.md"), undefined);
  assert.equal(levelOf("releasing.md"), "contributor");
});

test("example pages are guides", () => {
  assert.equal(levelOf("examples/01-start-here.md"), "guide");
  assert.equal(levelOf("examples.md"), "guide");
});
