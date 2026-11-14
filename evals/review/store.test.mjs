// node --test evals/review/*.test.mjs
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { lock, mismatched, readResults, writeResults } from "./store.mjs";

const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), "review-store-"));

test("a missing results file is empty; a damaged one is an error, not a fresh start", () => {
  const d = tmp();
  assert.deepEqual(readResults(d), []);
  writeResults(d, [{ case: "a", run: 1 }]);
  assert.deepEqual(readResults(d), [{ case: "a", run: 1 }]);
  assert.deepEqual(fs.readdirSync(d), ["results.json"], "no temp file left");
  fs.writeFileSync(path.join(d, "results.json"), '[{"case": "a", ');
  assert.throws(() => readResults(d), /does not parse/);
  fs.writeFileSync(path.join(d, "results.json"), "{}");
  assert.throws(() => readResults(d), /not an array/);
});

test("one invocation at a time per out directory; a dead holder's lock is taken over", () => {
  const d = tmp();
  const release = lock(d);
  assert.throws(() => lock(d), /in use by process/);
  release();
  const again = lock(d);
  again();
  fs.writeFileSync(path.join(d, ".lock"), "999999999");
  lock(d)();
});

test("entries from another prompt or model are mismatched", () => {
  const es = [{ promptSha: "a", model: "opus" }, { promptSha: "b", model: "opus" }, { promptSha: "a", model: "sonnet" }, {}];
  assert.equal(mismatched(es, { promptSha: "a", model: "opus" }).length, 2);
  assert.equal(mismatched([], { promptSha: "a", model: "opus" }).length, 0);
});
