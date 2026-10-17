/**
 * `ServerConnection` in the spirit of `@jupyterlab/services`.
 *
 * Everything that talks to the server goes through the `fetch` and
 * `WebSocket` carried in the settings object. On desktop and server
 * profiles they are the browser globals. On the iPad profile the app
 * injects a bridge implementation that carries the same JSON shapes to an
 * in-process Rust core with no HTTP in between, the same trick JupyterLite
 * uses to point JupyterLab at an in-browser backend.
 */

/**
 * Makes a function-typed property bivariant in its parameter, so the DOM
 * `WebSocket` (whose handlers take `MessageEvent` etc.) and a test fake
 * (whose handlers take plain objects) are both assignable.
 */
export type Handler<E> = { bivarianceHack(event: E): void }["bivarianceHack"];

/** The subset of the WHATWG WebSocket surface this package uses. */
export interface WebSocketLike {
  readonly readyState: number;
  binaryType: string;
  onopen: Handler<unknown> | null;
  onmessage: Handler<{ data: unknown }> | null;
  onclose: Handler<{ code?: number; reason?: string; wasClean?: boolean }> | null;
  onerror: Handler<unknown> | null;
  send(data: string | ArrayBuffer | ArrayBufferView): void;
  close(code?: number, reason?: string): void;
}

export interface WebSocketConstructor {
  new (url: string | URL, protocols?: string | string[]): WebSocketLike;
}

export interface ServerConnection {
  /** Always ends with "/". */
  baseUrl: string;
  /** Always ends with "/". Derived from `baseUrl` unless overridden. */
  wsUrl: string;
  token?: string;
  fetch: typeof fetch;
  WebSocket: WebSocketConstructor;
}

export type ServerConnectionOverrides = Partial<ServerConnection>;

function withTrailingSlash(url: string): string {
  return url.endsWith("/") ? url : `${url}/`;
}

function defaultBaseUrl(): string {
  const loc = (globalThis as { location?: { origin?: string } }).location;
  if (loc?.origin && loc.origin !== "null") {
    return `${loc.origin}/`;
  }
  return "http://localhost:8888/";
}

export function makeSettings(overrides: ServerConnectionOverrides = {}): ServerConnection {
  const baseUrl = withTrailingSlash(overrides.baseUrl ?? defaultBaseUrl());
  const wsUrl = withTrailingSlash(overrides.wsUrl ?? baseUrl.replace(/^http(s?):/, "ws$1:"));
  const globals = globalThis as { fetch?: typeof fetch; WebSocket?: WebSocketConstructor };
  const fetchImpl = overrides.fetch ?? globals.fetch?.bind(globalThis);
  if (!fetchImpl) {
    throw new Error("makeSettings: no fetch available; pass one in overrides");
  }
  const WebSocketImpl = overrides.WebSocket ?? globals.WebSocket;
  if (!WebSocketImpl) {
    throw new Error("makeSettings: no WebSocket available; pass one in overrides");
  }
  const settings: ServerConnection = { baseUrl, wsUrl, fetch: fetchImpl, WebSocket: WebSocketImpl };
  if (overrides.token !== undefined) {
    settings.token = overrides.token;
  }
  return settings;
}

/** Join a path onto `baseUrl`, keeping "/" separators but encoding each segment. */
export function apiUrl(settings: ServerConnection, path: string, query?: Record<string, string | undefined>): string {
  const encoded = path
    .replace(/^\/+/, "")
    .split("/")
    .map((seg) => encodeURIComponent(seg))
    .join("/");
  const url = new URL(encoded, settings.baseUrl);
  if (query) {
    for (const [key, value] of Object.entries(query)) {
      if (value !== undefined) url.searchParams.set(key, value);
    }
  }
  return url.toString();
}

export class ServerError extends Error {
  constructor(
    readonly status: number,
    readonly url: string,
    readonly body: string,
  ) {
    super(`${status} from ${url}: ${body.slice(0, 200)}`);
    this.name = "ServerError";
  }
}

// ---------------------------------------------------------------------------
// Response models. Deliberately loose: the server owns these shapes.
// ---------------------------------------------------------------------------

export interface KernelspecModel {
  name: string;
  spec: {
    display_name: string;
    language: string;
    argv?: string[];
    env?: Record<string, string>;
    metadata?: Record<string, unknown>;
    [key: string]: unknown;
  };
  resources: Record<string, string>;
  [key: string]: unknown;
}

export interface KernelspecsResponse {
  default: string;
  kernelspecs: Record<string, KernelspecModel>;
  [key: string]: unknown;
}

export interface KernelModel {
  id: string;
  name: string;
  last_activity?: string;
  execution_state?: string;
  connections?: number;
  [key: string]: unknown;
}

export interface SessionModel {
  id: string;
  path: string;
  name: string;
  type: string;
  kernel: KernelModel | null;
  [key: string]: unknown;
}

export interface CreateSessionRequest {
  path: string;
  name?: string;
  type?: string;
  kernel: { name?: string; id?: string };
  [key: string]: unknown;
}

export interface ContentsModel {
  name: string;
  path: string;
  type: "notebook" | "file" | "directory" | (string & {});
  format: "json" | "text" | "base64" | null;
  content: unknown;
  mimetype?: string | null;
  writable?: boolean;
  created?: string;
  last_modified?: string;
  [key: string]: unknown;
}

export interface GetContentsOptions {
  /** Set to false to fetch only metadata. */
  content?: boolean;
  type?: string;
  format?: string;
}

// ---------------------------------------------------------------------------
// REST client
// ---------------------------------------------------------------------------

export class RestClient {
  constructor(readonly settings: ServerConnection) {}

  /** One request through `settings.fetch`, with the token header and JSON handling. */
  async request<T>(
    path: string,
    init: { method?: string; body?: unknown; query?: Record<string, string | undefined> } = {},
  ): Promise<T> {
    const url = apiUrl(this.settings, path, init.query);
    const headers: Record<string, string> = {};
    if (this.settings.token) {
      headers["Authorization"] = `token ${this.settings.token}`;
    }
    let body: string | undefined;
    if (init.body !== undefined) {
      headers["Content-Type"] = "application/json";
      body = JSON.stringify(init.body);
    }
    const response = await this.settings.fetch(url, { method: init.method ?? "GET", headers, body });
    if (!response.ok) {
      throw new ServerError(response.status, url, await response.text());
    }
    if (response.status === 204) {
      return undefined as T;
    }
    const text = await response.text();
    return (text.length ? JSON.parse(text) : undefined) as T;
  }

  listKernelspecs(): Promise<KernelspecsResponse> {
    return this.request("api/kernelspecs");
  }

  listKernels(): Promise<KernelModel[]> {
    return this.request("api/kernels");
  }

  getKernel(id: string): Promise<KernelModel> {
    return this.request(`api/kernels/${id}`);
  }

  startKernel(name?: string, path?: string): Promise<KernelModel> {
    const body: Record<string, string> = {};
    if (name !== undefined) body["name"] = name;
    if (path !== undefined) body["path"] = path;
    return this.request("api/kernels", { method: "POST", body });
  }

  shutdownKernel(id: string): Promise<void> {
    return this.request(`api/kernels/${id}`, { method: "DELETE" });
  }

  interruptKernel(id: string): Promise<void> {
    return this.request(`api/kernels/${id}/interrupt`, { method: "POST" });
  }

  restartKernel(id: string): Promise<KernelModel> {
    return this.request(`api/kernels/${id}/restart`, { method: "POST" });
  }

  listSessions(): Promise<SessionModel[]> {
    return this.request("api/sessions");
  }

  getSession(id: string): Promise<SessionModel> {
    return this.request(`api/sessions/${id}`);
  }

  createSession(model: CreateSessionRequest): Promise<SessionModel> {
    return this.request("api/sessions", { method: "POST", body: model });
  }

  deleteSession(id: string): Promise<void> {
    return this.request(`api/sessions/${id}`, { method: "DELETE" });
  }

  getContents(path: string, options: GetContentsOptions = {}): Promise<ContentsModel> {
    const query: Record<string, string | undefined> = {};
    if (options.content === false) query["content"] = "0";
    if (options.type) query["type"] = options.type;
    if (options.format) query["format"] = options.format;
    return this.request(`api/contents/${path}`, { query });
  }

  saveContents(path: string, model: Partial<ContentsModel>): Promise<ContentsModel> {
    return this.request(`api/contents/${path}`, { method: "PUT", body: model });
  }
}
