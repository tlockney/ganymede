import { test } from "node:test";
import assert from "node:assert/strict";
import { createMessage, isMessageType, PROTOCOL_VERSION } from "./messages.js";

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

test("createMessage builds a 5.5 envelope", () => {
  const msg = createMessage("execute_request", { code: "1" }, { session: "sess", username: "me" });
  assert.match(msg.header.msg_id, UUID);
  assert.equal(msg.header.session, "sess");
  assert.equal(msg.header.username, "me");
  assert.equal(msg.header.msg_type, "execute_request");
  assert.equal(msg.header.version, PROTOCOL_VERSION);
  assert.equal(msg.header.version, "5.5");
  assert.ok(!Number.isNaN(Date.parse(msg.header.date)));
  assert.match(msg.header.date, /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/);
  assert.ok(!("subshell_id" in msg.header));
  assert.deepEqual(msg.parent_header, {});
  assert.deepEqual(msg.metadata, {});
  assert.deepEqual(msg.content, { code: "1" });
  assert.deepEqual(msg.buffers, []);
});

test("createMessage copies the parent header and takes options", () => {
  const parent = createMessage("input_request", { prompt: ">", password: false }, { session: "kernel" });
  const buffer = new ArrayBuffer(3);
  const msg = createMessage(
    "input_reply",
    { value: "x" },
    { session: "sess", parent, metadata: { a: 1 }, buffers: [buffer], subshellId: "sub-1" },
  );
  assert.deepEqual(msg.parent_header, parent.header);
  assert.notEqual(msg.parent_header, parent.header, "parent header is copied, not shared");
  assert.equal(msg.header.username, "username");
  assert.equal(msg.header.subshell_id, "sub-1");
  assert.deepEqual(msg.metadata, { a: 1 });
  assert.equal(msg.buffers[0], buffer);
  assert.notEqual(msg.header.msg_id, parent.header.msg_id);
});

test("isMessageType narrows by msg_type", () => {
  const msg = createMessage("status", { execution_state: "idle" }, { session: "s" });
  assert.ok(isMessageType(msg, "status"));
  assert.ok(!isMessageType(msg, "stream"));
});
