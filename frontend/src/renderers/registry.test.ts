import { test } from "node:test";
import assert from "node:assert/strict";
import { DEFAULT_MIME_ORDER, RendererRegistry, WIDGET_VIEW_MIME, defaultRank, type MimeRenderer } from "./registry.js";

function fake(mime: string, rank = defaultRank(mime)): MimeRenderer {
  return { mimeTypes: [mime], rank, render: () => undefined };
}

function registryWith(...mimes: string[]): RendererRegistry {
  const registry = new RendererRegistry();
  for (const mime of mimes) registry.register(fake(mime));
  return registry;
}

test("default order matches sketch.md 2.6", () => {
  assert.deepEqual(DEFAULT_MIME_ORDER, [
    WIDGET_VIEW_MIME,
    "text/html",
    "image/svg+xml",
    "image/png",
    "image/jpeg",
    "text/markdown",
    "text/latex",
    "application/json",
    "text/plain",
  ]);
  assert.ok(defaultRank("text/html") < defaultRank("image/png"));
  assert.ok(defaultRank("image/png") < defaultRank("text/plain"));
  assert.ok(defaultRank("application/x-unknown") > defaultRank("text/plain"));
});

test("preferredMimeType picks the best ranked type present", () => {
  const registry = registryWith(...DEFAULT_MIME_ORDER);
  assert.equal(registry.preferredMimeType({ "text/plain": "x", "image/png": "..." }), "image/png");
  assert.equal(registry.preferredMimeType({ "text/plain": "x", "text/html": "<b/>", "image/png": "" }), "text/html");
  assert.equal(registry.preferredMimeType({ "text/plain": "x", [WIDGET_VIEW_MIME]: { model_id: "m" } }), WIDGET_VIEW_MIME);
  assert.equal(registry.preferredMimeType({ "text/plain": "x" }), "text/plain");
});

test("types without a renderer are skipped", () => {
  const registry = registryWith("text/plain", "image/png");
  assert.equal(registry.preferredMimeType({ "text/html": "<b/>", "text/plain": "x" }), "text/plain");
  assert.equal(registry.preferredMimeType({ "application/vnd.something": {} }), undefined);
  assert.equal(registry.preferredMimeType({}), undefined);
});

test("a custom rank overrides the default order", () => {
  const registry = registryWith("text/html", "image/png");
  registry.register(fake("text/plain", -1));
  assert.equal(registry.preferredMimeType({ "text/html": "", "text/plain": "", "image/png": "" }), "text/plain");
});

test("register returns an unregister function and keeps the best renderer per type", () => {
  const registry = new RendererRegistry();
  const low = fake("text/plain", 5);
  const high = fake("text/plain", 1);
  const offLow = registry.register(low);
  registry.register(high);
  assert.equal(registry.rendererFor("text/plain"), high);
  offLow();
  assert.equal(registry.rendererFor("text/plain"), high);
  registry.register(low);
  assert.deepEqual(registry.mimeTypes(), ["text/plain"]);
});
