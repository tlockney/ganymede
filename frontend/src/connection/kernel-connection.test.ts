import { test } from "node:test";
import assert from "node:assert/strict";
import { createMessage, type Channel, type JsonObject, type Message, type MessageType } from "../protocol/messages.js";
import { WS_SUBPROTOCOL_V1, decodeV1, encodeV1, type ChannelMessage } from "../protocol/websocket-v1.js";
import { KernelConnection } from "./kernel-connection.js";
import { makeSettings, type Handler, type WebSocketLike } from "./server-connection.js";

/** A WebSocket whose server side is driven by the test. */
class FakeWebSocket implements WebSocketLike {
  static instances: FakeWebSocket[] = [];

  readyState = 0;
  binaryType = "blob";
  onopen: Handler<unknown> | null = null;
  onmessage: Handler<{ data: unknown }> | null = null;
  onclose: Handler<{ code?: number; reason?: string; wasClean?: boolean }> | null = null;
  onerror: Handler<unknown> | null = null;

  readonly url: string;
  readonly protocols: string[] | undefined;
  private readonly queue: ArrayBuffer[] = [];
  private readonly waiters: ((frame: ArrayBuffer) => void)[] = [];

  constructor(url: string | URL, protocols?: string | string[]) {
    this.url = String(url);
    this.protocols = protocols === undefined ? undefined : ([] as string[]).concat(protocols);
    FakeWebSocket.instances.push(this);
    queueMicrotask(() => {
      this.readyState = 1;
      this.onopen?.({});
    });
  }

  send(data: string | ArrayBuffer | ArrayBufferView): void {
    if (typeof data === "string") throw new Error("fake expected a binary v1 frame");
    const frame = ArrayBuffer.isView(data)
      ? data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength)
      : data;
    const waiter = this.waiters.shift();
    if (waiter) waiter(frame as ArrayBuffer);
    else this.queue.push(frame as ArrayBuffer);
  }

  close(code = 1000, reason = ""): void {
    this.readyState = 3;
    this.onclose?.({ code, reason, wasClean: true });
  }

  /** Next frame the client sends, decoded. */
  nextSent(): Promise<ChannelMessage> {
    const queued = this.queue.shift();
    if (queued) return Promise.resolve(decodeV1(queued));
    return new Promise((resolve) => this.waiters.push((frame) => resolve(decodeV1(frame))));
  }

  /** Deliver a message from the "kernel". */
  serverSend(channel: Channel, message: Message<unknown>): void {
    this.onmessage?.({ data: encodeV1(channel, message) });
  }
}

function fromKernel<C>(msgType: MessageType, content: C, parent?: Message<unknown>): Message<C> {
  const options: Parameters<typeof createMessage>[2] = { session: "kernel-session" };
  if (parent) options.parent = parent;
  return createMessage(msgType, content, options);
}

async function connect(): Promise<{ kc: KernelConnection; ws: FakeWebSocket }> {
  const settings = makeSettings({ baseUrl: "http://localhost:8888/", token: "tok", WebSocket: FakeWebSocket });
  const kc = new KernelConnection(settings, "k1", { session: "client-session", username: "me" });
  const ws = FakeWebSocket.instances[FakeWebSocket.instances.length - 1]!;
  await Promise.resolve();
  return { kc, ws };
}

/** True if `promise` settles before a macrotask tick. */
async function settledSoon(promise: Promise<unknown>): Promise<boolean> {
  const tick = new Promise<boolean>((resolve) => setTimeout(() => resolve(false), 0));
  return Promise.race([promise.then(() => true, () => true), tick]);
}

test("opens the channels websocket with the v1 subprotocol and session id", async () => {
  const { kc, ws } = await connect();
  assert.equal(ws.url, "ws://localhost:8888/api/kernels/k1/channels?session_id=client-session&token=tok");
  assert.deepEqual(ws.protocols, [WS_SUBPROTOCOL_V1]);
  assert.equal(ws.binaryType, "arraybuffer");
  assert.equal(kc.session, "client-session");
  kc.dispose();
});

test("execute: done resolves after execute_reply then status idle", async () => {
  const { kc, ws } = await connect();
  const future = kc.execute("1+1");
  const seen: string[] = [];
  future.onIOPub((m) => seen.push(m.header.msg_type));

  const sent = await ws.nextSent();
  assert.equal(sent.channel, "shell");
  assert.equal(sent.message.header.msg_type, "execute_request");
  assert.equal(sent.message.header.session, "client-session");
  assert.deepEqual(sent.message.content, {
    code: "1+1",
    silent: false,
    store_history: true,
    user_expressions: {},
    allow_stdin: true,
    stop_on_error: true,
  });
  const request = sent.message;
  assert.equal(future.msgId, request.header.msg_id);

  ws.serverSend("iopub", fromKernel("status", { execution_state: "busy" }, request));
  ws.serverSend("iopub", fromKernel("execute_result", { data: { "text/plain": "2" }, metadata: {}, execution_count: 1 }, request));
  ws.serverSend("shell", fromKernel("execute_reply", { status: "ok", execution_count: 1 }, request));
  assert.equal(await settledSoon(future.done), false, "reply alone is not done");

  // An idle for some other request must not count.
  ws.serverSend("iopub", fromKernel("status", { execution_state: "idle" }, fromKernel("kernel_info_request", {})));
  assert.equal(await settledSoon(future.done), false, "unrelated idle is not done");

  ws.serverSend("iopub", fromKernel("status", { execution_state: "idle" }, request));
  const reply = await future.done;
  assert.equal(reply.content.status, "ok");
  assert.equal(reply.content.execution_count, 1);
  assert.deepEqual(seen, ["status", "execute_result", "status"]);
  assert.equal(kc.status, "idle");
  kc.dispose();
});

test("execute: done resolves after status idle then execute_reply", async () => {
  const { kc, ws } = await connect();
  const future = kc.execute("raise", { stop_on_error: false });
  const request = (await ws.nextSent()).message;
  assert.equal((request.content as JsonObject)["stop_on_error"], false);

  ws.serverSend("iopub", fromKernel("status", { execution_state: "busy" }, request));
  ws.serverSend("iopub", fromKernel("error", { ename: "E", evalue: "v", traceback: [] }, request));
  ws.serverSend("iopub", fromKernel("status", { execution_state: "idle" }, request));
  assert.equal(await settledSoon(future.done), false, "idle alone is not done");

  ws.serverSend("shell", fromKernel("execute_reply", { status: "error", execution_count: 2, ename: "E", evalue: "v", traceback: [] }, request));
  const reply = await future.done;
  assert.equal(reply.content.status, "error", "an error reply still resolves done");
  kc.dispose();
});

test("execute: done rejects when the socket closes first", async () => {
  const { kc, ws } = await connect();
  const future = kc.execute("sleep");
  await ws.nextSent();
  ws.close(1006, "gone");
  await assert.rejects(future.done, /closed \(1006\) gone/);
  assert.equal(kc.status, "closed");
});

test("requestReply matches on parent msg_id and channel", async () => {
  const { kc, ws } = await connect();
  const pending = kc.kernelInfo();
  const sent = await ws.nextSent();
  assert.equal(sent.message.header.msg_type, "kernel_info_request");

  // Wrong parent and wrong channel are ignored.
  ws.serverSend("shell", fromKernel("kernel_info_reply", { status: "ok", banner: "no" }, fromKernel("kernel_info_request", {})));
  ws.serverSend("iopub", fromKernel("status", { execution_state: "busy" }, sent.message));
  assert.equal(await settledSoon(pending), false);

  ws.serverSend(
    "shell",
    fromKernel(
      "kernel_info_reply",
      { status: "ok", banner: "yes", language_info: { name: "typescript" }, capabilities: { subshells: true } },
      sent.message,
    ),
  );
  const reply = await pending;
  assert.equal(reply.content.banner, "yes");
  assert.deepEqual(reply.content["capabilities"], { subshells: true }, "unknown fields survive");
  kc.dispose();
});

test("inputReply answers the pending input_request on stdin", async () => {
  const { kc, ws } = await connect();
  await assert.rejects(kc.inputReply("x"), /no pending input_request/);

  const request = fromKernel("input_request", { prompt: "name? ", password: false });
  ws.serverSend("stdin", request);
  assert.equal(kc.pendingInputRequest?.content.prompt, "name? ");

  await kc.inputReply("ada");
  const sent = await ws.nextSent();
  assert.equal(sent.channel, "stdin");
  assert.equal(sent.message.header.msg_type, "input_reply");
  assert.deepEqual(sent.message.content, { value: "ada" });
  assert.equal(sent.message.parent_header.msg_id, request.header.msg_id);
  assert.equal(kc.pendingInputRequest, undefined);
  kc.dispose();
});

test("onMessage listeners see every channel and can unsubscribe", async () => {
  const { kc, ws } = await connect();
  const seen: string[] = [];
  const off = kc.onMessage((channel, message) => seen.push(`${channel}:${message.header.msg_type}`));
  ws.serverSend("iopub", fromKernel("status", { execution_state: "starting" }));
  ws.serverSend("control", fromKernel("interrupt_reply", { status: "ok" }));
  off();
  ws.serverSend("iopub", fromKernel("status", { execution_state: "idle" }));
  assert.deepEqual(seen, ["iopub:status", "control:interrupt_reply"]);
  kc.dispose();
});
