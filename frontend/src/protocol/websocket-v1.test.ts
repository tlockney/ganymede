import { test } from "node:test";
import assert from "node:assert/strict";
import { createMessage, type Message } from "./messages.js";
import { WS_SUBPROTOCOL_V1, decodeLegacy, decodeV1, encodeLegacy, encodeV1 } from "./websocket-v1.js";

function bytesOf(buffer: ArrayBuffer): number[] {
  return [...new Uint8Array(buffer)];
}

function readOffsets(frame: ArrayBuffer): { count: number; offsets: number[] } {
  const view = new DataView(frame);
  const count = Number(view.getBigUint64(0, true));
  const offsets: number[] = [];
  for (let i = 0; i < count; i++) offsets.push(Number(view.getBigUint64(8 * (1 + i), true)));
  return { count, offsets };
}

test("subprotocol name", () => {
  assert.equal(WS_SUBPROTOCOL_V1, "v1.kernel.websocket.jupyter.org");
});

test("v1 round trip with two binary buffers", () => {
  const b1 = new Uint8Array([1, 2, 3, 255]).buffer;
  const b2 = new Uint8Array([0, 9]).buffer;
  const parent = createMessage("execute_request", { code: "x" }, { session: "client" });
  const original = createMessage(
    "comm_msg",
    { comm_id: "c1", data: { method: "update", state: { n: 2 } } },
    { session: "kernel", parent, metadata: { version: "2.1.0" }, buffers: [b1, b2] },
  );

  const frame = encodeV1("iopub", original);
  const { channel, message } = decodeV1(frame);

  assert.equal(channel, "iopub");
  assert.deepEqual(message.header, original.header);
  assert.deepEqual(message.parent_header, original.parent_header);
  assert.deepEqual(message.metadata, original.metadata);
  assert.deepEqual(message.content, original.content);
  assert.equal(message.buffers.length, 2);
  assert.ok(message.buffers[0] instanceof ArrayBuffer);
  assert.deepEqual(bytesOf(message.buffers[0]!), [1, 2, 3, 255]);
  assert.deepEqual(bytesOf(message.buffers[1]!), [0, 9]);
  assert.notEqual(message.buffers[0], b1, "buffers are copied out of the frame");

  // Layout: 1 count + (2 + 6 parts) offsets = 9 u64 = 72 bytes of prefix.
  const { count, offsets } = readOffsets(frame);
  assert.equal(count, 8);
  assert.equal(offsets[0], 8 * (1 + 1 + 6 + 1));
  assert.equal(offsets[1], offsets[0]! + "iopub".length);
  assert.equal(offsets[offsets.length - 1], frame.byteLength);
  assert.equal(offsets[7]! - offsets[6]!, 2, "last buffer is two bytes");
});

test("v1 round trip with non-ASCII content and no buffers", () => {
  const original = createMessage(
    "stream",
    { name: "stdout", text: "héllo — 🌍 \u{1F600} ✓\n" },
    { session: "kernel", username: "ünïcode" },
  );
  const frame = encodeV1("shell", original);
  const { count, offsets } = readOffsets(frame);
  assert.equal(count, 6, "channel start, channel end, four part ends");
  assert.equal(offsets[0], 56);
  assert.equal(offsets[1], 56 + 5);
  assert.equal(offsets[5], frame.byteLength);
  // The header JSON is longer in bytes than in UTF-16 code units, so a
  // code-unit based encoder would have produced wrong offsets.
  const headerBytes = offsets[2]! - offsets[1]!;
  assert.equal(headerBytes, new TextEncoder().encode(JSON.stringify(original.header)).byteLength);

  const { channel, message } = decodeV1(frame);
  assert.equal(channel, "shell");
  assert.deepEqual(message, { ...original, buffers: [] });
});

test("v1 decode accepts a Uint8Array view at a non-zero offset", () => {
  const original = createMessage("status", { execution_state: "busy" }, { session: "k" });
  const frame = new Uint8Array(encodeV1("iopub", original));
  const padded = new Uint8Array(frame.length + 7);
  padded.set(frame, 7);
  const view = padded.subarray(7);
  const { channel, message } = decodeV1(view);
  assert.equal(channel, "iopub");
  assert.deepEqual(message.content, { execution_state: "busy" });
});

test("v1 decode rejects malformed frames", () => {
  assert.throws(() => decodeV1(new ArrayBuffer(4)), /shorter/);
  const bogus = new ArrayBuffer(16);
  new DataView(bogus).setBigUint64(0, 1n, true);
  assert.throws(() => decodeV1(bogus), /invalid offset count/);

  const good = encodeV1("shell", createMessage("x", {}, { session: "s" }));
  const truncated = good.slice(0, good.byteLength - 1);
  assert.throws(() => decodeV1(truncated), /last offset/);

  const badChannel = encodeV1("shell", createMessage("x", {}, { session: "s" }));
  new Uint8Array(badChannel).set(new TextEncoder().encode("shelx"), 56);
  assert.throws(() => decodeV1(badChannel), /unknown channel/);
});

test("legacy JSON round trip", () => {
  const parent = createMessage("execute_request", { code: "1" }, { session: "c" });
  const original = createMessage("execute_reply", { status: "ok", execution_count: 1 }, { session: "k", parent });
  const text = encodeLegacy("shell", original);
  const json = JSON.parse(text) as Record<string, unknown>;
  assert.equal(json["channel"], "shell");
  assert.deepEqual(Object.keys(json).sort(), ["channel", "content", "header", "metadata", "parent_header"]);

  const { channel, message } = decodeLegacy(text);
  assert.equal(channel, "shell");
  assert.deepEqual(message, { ...original, buffers: [] });
});

test("legacy codec refuses buffers", () => {
  const withBuffers: Message = createMessage("comm_msg", {}, { session: "k", buffers: [new ArrayBuffer(1)] });
  assert.throws(() => encodeLegacy("iopub", withBuffers), /buffers/);
  assert.throws(
    () => decodeLegacy(JSON.stringify({ channel: "iopub", header: {}, content: {}, buffers: [1] })),
    /buffers/,
  );
  assert.throws(() => decodeLegacy(JSON.stringify({ channel: "nope", header: {} })), /unknown channel/);
});
