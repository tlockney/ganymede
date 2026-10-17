import { test } from "node:test";
import assert from "node:assert/strict";
import { ansiToText, parseAnsi } from "./ansi.js";

test("plain text is one unstyled span", () => {
  assert.deepEqual(parseAnsi("hello"), [{ text: "hello", classes: [] }]);
  assert.deepEqual(parseAnsi(""), []);
});

test("bold and reset", () => {
  assert.deepEqual(parseAnsi("a\x1b[1mb\x1b[0mc"), [
    { text: "a", classes: [] },
    { text: "b", classes: ["ansi-bold"] },
    { text: "c", classes: [] },
  ]);
});

test("bare ESC[m is a reset", () => {
  assert.deepEqual(parseAnsi("\x1b[31mx\x1b[my"), [
    { text: "x", classes: ["ansi-fg-red"] },
    { text: "y", classes: [] },
  ]);
});

test("8 colors, background, bright colors, combined params", () => {
  assert.deepEqual(parseAnsi("\x1b[32mgreen\x1b[39m plain"), [
    { text: "green", classes: ["ansi-fg-green"] },
    { text: " plain", classes: [] },
  ]);
  assert.deepEqual(parseAnsi("\x1b[1;44;97mX\x1b[0m"), [
    { text: "X", classes: ["ansi-bold", "ansi-fg-bright-white", "ansi-bg-blue"] },
  ]);
  assert.deepEqual(parseAnsi("\x1b[101mX"), [{ text: "X", classes: ["ansi-bg-bright-red"] }]);
});

test("256-color foreground and background", () => {
  assert.deepEqual(parseAnsi("\x1b[38;5;208morange\x1b[48;5;17m!"), [
    { text: "orange", classes: ["ansi-fg-208"] },
    { text: "!", classes: ["ansi-fg-208", "ansi-bg-17"] },
  ]);
});

test("truecolor is consumed without breaking later params", () => {
  assert.deepEqual(parseAnsi("\x1b[38;2;10;20;30;1mX"), [{ text: "X", classes: ["ansi-bold"] }]);
});

test("22/24 undo bold and underline", () => {
  assert.deepEqual(parseAnsi("\x1b[1;4ma\x1b[22mb\x1b[24mc"), [
    { text: "a", classes: ["ansi-bold", "ansi-underline"] },
    { text: "b", classes: ["ansi-underline"] },
    { text: "c", classes: [] },
  ]);
});

test("non-SGR CSI sequences are stripped", () => {
  assert.deepEqual(parseAnsi("a\x1b[2Kb\x1b[1Ac"), [{ text: "abc", classes: [] }]);
});

test("carriage return overwrites the current line", () => {
  assert.equal(ansiToText("abc\rX"), "Xbc");
  assert.equal(ansiToText("progress 10%\rprogress 100%"), "progress 100%");
  assert.equal(ansiToText("line1\nabc\rXY\nline3"), "line1\nXYc\nline3");
  assert.equal(ansiToText("abc\r"), "abc", "trailing CR leaves the line intact");
  assert.equal(ansiToText("a\r\nb"), "a\nb", "CRLF is a newline");
});

test("carriage return keeps styles of overwritten text", () => {
  assert.deepEqual(parseAnsi("\x1b[31mabc\x1b[0m\rX"), [
    { text: "X", classes: [] },
    { text: "bc", classes: ["ansi-fg-red"] },
  ]);
});

test("backspace deletes the previous character", () => {
  assert.equal(ansiToText("abc\bd"), "abd");
  assert.equal(ansiToText("abc\b\b"), "a");
  assert.equal(ansiToText("\bx"), "x", "backspace at line start is a no-op");
  assert.equal(ansiToText("ab\n\bc"), "ab\nc", "backspace does not cross a newline");
});

test("style spans merge across newlines", () => {
  assert.deepEqual(parseAnsi("\x1b[1mab\ncd\x1b[0m"), [{ text: "ab\ncd", classes: ["ansi-bold"] }]);
});

test("astral characters are single cells", () => {
  assert.equal(ansiToText("🌍x\b\b!"), "!");
});
