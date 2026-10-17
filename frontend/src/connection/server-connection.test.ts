import { test } from "node:test";
import assert from "node:assert/strict";
import { RestClient, apiUrl, makeSettings, type WebSocketLike } from "./server-connection.js";

class NoopWebSocket implements WebSocketLike {
  readyState = 0;
  binaryType = "blob";
  onopen = null;
  onmessage = null;
  onclose = null;
  onerror = null;
  constructor(readonly url: string | URL) {}
  send(): void {}
  close(): void {}
}

test("makeSettings normalizes urls and defaults to globals", () => {
  const settings = makeSettings({ baseUrl: "http://host:1234/prefix" });
  assert.equal(settings.baseUrl, "http://host:1234/prefix/");
  assert.equal(settings.wsUrl, "ws://host:1234/prefix/");
  assert.equal(typeof settings.fetch, "function");
  assert.equal(typeof settings.WebSocket, "function");
  assert.equal(settings.token, undefined);

  const https = makeSettings({ baseUrl: "https://h/", token: "abc", WebSocket: NoopWebSocket });
  assert.equal(https.wsUrl, "wss://h/");
  assert.equal(https.WebSocket, NoopWebSocket);
  assert.equal(https.token, "abc");
});

test("apiUrl encodes segments and keeps slashes", () => {
  const settings = makeSettings({ baseUrl: "http://h/base" });
  assert.equal(apiUrl(settings, "api/contents/dir name/nb#1.ipynb"), "http://h/base/api/contents/dir%20name/nb%231.ipynb");
  assert.equal(apiUrl(settings, "/api/kernels", { a: "1", b: undefined }), "http://h/base/api/kernels?a=1");
});

test("RestClient sends the token header and hits jupyter_server paths", async () => {
  const calls: { url: string; init: RequestInit }[] = [];
  const fakeFetch: typeof fetch = async (input, init) => {
    calls.push({ url: String(input), init: init ?? {} });
    const url = String(input);
    if (url.endsWith("/api/kernels") && init?.method === "POST") {
      return new Response(JSON.stringify({ id: "k1", name: "cdp" }), { status: 201 });
    }
    if (url.includes("/interrupt")) return new Response(null, { status: 204 });
    if (url.includes("/api/contents/")) {
      return new Response(JSON.stringify({ name: "a.ipynb", path: "a.ipynb", type: "notebook", content: null }));
    }
    return new Response("[]");
  };
  const client = new RestClient(makeSettings({ baseUrl: "http://h/", token: "tok", fetch: fakeFetch, WebSocket: NoopWebSocket }));

  assert.deepEqual(await client.listKernels(), []);
  assert.equal(calls[0]!.url, "http://h/api/kernels");
  assert.equal((calls[0]!.init.headers as Record<string, string>)["Authorization"], "token tok");

  const kernel = await client.startKernel("cdp", "notebooks");
  assert.equal(kernel.id, "k1");
  assert.equal(calls[1]!.init.body, JSON.stringify({ name: "cdp", path: "notebooks" }));

  await client.interruptKernel("k1");
  assert.equal(calls[2]!.url, "http://h/api/kernels/k1/interrupt");
  assert.equal(calls[2]!.init.method, "POST");

  const contents = await client.getContents("sub/a.ipynb", { content: false });
  assert.equal(contents.type, "notebook");
  assert.equal(calls[3]!.url, "http://h/api/contents/sub/a.ipynb?content=0");

  await client.saveContents("sub/a.ipynb", { type: "notebook", format: "json", content: {} });
  assert.equal(calls[4]!.init.method, "PUT");
});

test("RestClient throws ServerError on non-2xx", async () => {
  const fakeFetch: typeof fetch = async () => new Response("nope", { status: 403 });
  const client = new RestClient(makeSettings({ baseUrl: "http://h/", fetch: fakeFetch, WebSocket: NoopWebSocket }));
  await assert.rejects(client.listKernelspecs(), (error: unknown) => {
    assert.ok(error instanceof Error);
    assert.equal(error.name, "ServerError");
    assert.match(error.message, /403/);
    return true;
  });
});
