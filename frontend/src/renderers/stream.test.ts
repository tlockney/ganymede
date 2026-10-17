import { test } from "node:test";
import assert from "node:assert/strict";
import { StreamBuffer, applyControlChars } from "./stream.js";

test("applyControlChars handles CR, backspace and newlines", () => {
  assert.equal(applyControlChars("abc\rX"), "Xbc");
  assert.equal(applyControlChars("abc\bd\n"), "abd\n");
  assert.equal(applyControlChars("a\r\nb"), "a\nb");
  assert.equal(applyControlChars("10%\r20%\r30%"), "30%");
});

test("consecutive chunks of the same stream merge", () => {
  const buffer = new StreamBuffer();
  buffer.append("stdout", "hel");
  buffer.append("stdout", "lo\n");
  buffer.append("stdout", "world");
  assert.equal(buffer.length, 1);
  assert.deepEqual(buffer.entries, [{ name: "stdout", raw: "hello\nworld", text: "hello\nworld" }]);
});

test("a different stream starts a new entry", () => {
  const buffer = new StreamBuffer();
  buffer.append("stdout", "out1");
  buffer.append("stderr", "err");
  buffer.append("stdout", "out2");
  assert.equal(buffer.length, 3);
  assert.deepEqual(
    buffer.entries.map((e) => [e.name, e.text]),
    [
      ["stdout", "out1"],
      ["stderr", "err"],
      ["stdout", "out2"],
    ],
  );
  assert.equal(buffer.text("stdout"), "out1out2");
  assert.equal(buffer.text(), "out1errout2");
});

test("carriage return works across chunk boundaries", () => {
  const buffer = new StreamBuffer();
  buffer.append("stdout", "step 1/3");
  buffer.append("stdout", "\r");
  buffer.append("stdout", "step 2/3\r");
  const last = buffer.append("stdout", "step 3/3\n");
  assert.equal(last.text, "step 3/3\n");
  assert.equal(last.raw, "step 1/3\rstep 2/3\rstep 3/3\n");
});

test("backspace works across chunk boundaries", () => {
  const buffer = new StreamBuffer();
  buffer.append("stderr", "abc");
  buffer.append("stderr", "\b");
  const last = buffer.append("stderr", "d");
  assert.equal(last.text, "abd");
});

test("clear empties the buffer", () => {
  const buffer = new StreamBuffer();
  buffer.append("stdout", "x");
  buffer.clear();
  assert.equal(buffer.length, 0);
  assert.equal(buffer.text(), "");
});
