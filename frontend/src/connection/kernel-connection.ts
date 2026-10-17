/**
 * A live connection to one kernel over `/api/kernels/{id}/channels`.
 *
 * Owns the websocket, frames messages with the v1 subprotocol (falling back
 * to the legacy JSON codec when the server sends text frames), routes
 * replies to their requests by parent `msg_id`, and tracks the busy/idle
 * status the kernel publishes on iopub.
 *
 * TODO(reconnect): out of scope for the sketch. A reconnect needs to reopen
 * the socket with the same `session_id`, replay nothing (the server keeps
 * kernel state), and re-resolve the busy/idle status from the next status
 * message. Pending `requestReply` promises should be rejected on close, as
 * they are now, and callers retry.
 */

import {
  createMessage,
  type Channel,
  type ExecuteReplyContent,
  type ExecuteRequestContent,
  type ExecutionState,
  type InputReplyContent,
  type InputRequestContent,
  type JsonObject,
  type KernelInfoReplyContent,
  type Message,
  type MessageType,
  type StatusContent,
} from "../protocol/messages.js";
import {
  WS_SUBPROTOCOL_V1,
  decodeLegacy,
  decodeV1,
  encodeLegacy,
  encodeV1,
  type ChannelMessage,
} from "../protocol/websocket-v1.js";
import type { ServerConnection, WebSocketLike } from "./server-connection.js";

export type MessageListener = (channel: Channel, message: Message) => void;

export interface KernelConnectionOptions {
  /** Client session id. Defaults to a fresh UUID. */
  session?: string;
  username?: string;
  /** Wire format to send. The receive side accepts either. */
  protocol?: "v1" | "legacy";
}

export interface ExecuteOptions extends Partial<Omit<ExecuteRequestContent, "code">> {
  metadata?: JsonObject;
}

export interface ExecuteFuture {
  /** `msg_id` of the execute_request; iopub output carries it as parent. */
  readonly msgId: string;
  readonly request: Message<ExecuteRequestContent>;
  /** Receive iopub messages whose parent is this request. Returns an unsubscribe. */
  onIOPub(listener: (message: Message) => void): () => void;
  /**
   * Resolves once both the execute_reply and the iopub `status: idle` for
   * this request have arrived, in either order. The reply may have
   * `status: "error"`; that is still a resolution, not a rejection.
   */
  readonly done: Promise<Message<ExecuteReplyContent>>;
}

export class KernelConnection {
  readonly kernelId: string;
  readonly session: string;
  readonly username: string;
  readonly url: string;

  private readonly ws: WebSocketLike;
  private readonly ready: Promise<void>;
  private readonly listeners = new Set<MessageListener>();
  private readonly closeListeners = new Set<(reason: Error) => void>();
  private sendFormat: "v1" | "legacy";
  private lastInputRequest: Message<InputRequestContent> | undefined;
  private _status: ExecutionState | "unknown" | "closed" = "unknown";

  constructor(settings: ServerConnection, kernelId: string, options: KernelConnectionOptions = {}) {
    this.kernelId = kernelId;
    this.session = options.session ?? crypto.randomUUID();
    this.username = options.username ?? "username";
    this.sendFormat = options.protocol ?? "v1";

    const url = new URL(`api/kernels/${encodeURIComponent(kernelId)}/channels`, settings.wsUrl);
    url.searchParams.set("session_id", this.session);
    if (settings.token) {
      // Browsers cannot set headers on a websocket handshake, so jupyter_server
      // also accepts the token as a query parameter.
      url.searchParams.set("token", settings.token);
    }
    this.url = url.toString();

    const protocols = this.sendFormat === "v1" ? [WS_SUBPROTOCOL_V1] : [];
    this.ws = new settings.WebSocket(this.url, protocols);
    this.ws.binaryType = "arraybuffer";

    this.ready = new Promise<void>((resolve, reject) => {
      this.ws.onopen = () => resolve();
      this.ws.onerror = (event) => reject(new Error(`websocket error: ${describe(event)}`));
    });
    // Nobody may ever await `ready` if the socket fails before use.
    this.ready.catch(() => undefined);

    this.ws.onmessage = (event) => this.handleFrame(event.data);
    this.ws.onclose = (event) => {
      this._status = "closed";
      const reason = new Error(`websocket closed (${event.code ?? "?"}) ${event.reason ?? ""}`.trim());
      for (const listener of this.closeListeners) listener(reason);
      this.closeListeners.clear();
    };
  }

  /** Last `execution_state` seen on iopub, or "unknown" / "closed". */
  get status(): ExecutionState | "unknown" | "closed" {
    return this._status;
  }

  onMessage(listener: MessageListener): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  onClose(listener: (reason: Error) => void): () => void {
    this.closeListeners.add(listener);
    return () => {
      this.closeListeners.delete(listener);
    };
  }

  async send(channel: Channel, message: Message<unknown>): Promise<void> {
    await this.ready;
    if (this.sendFormat === "v1") {
      this.ws.send(encodeV1(channel, message));
    } else {
      this.ws.send(encodeLegacy(channel, message));
    }
  }

  /** Build a message stamped with this connection's session and username. */
  createMessage<C>(msgType: MessageType, content: C, parent?: Message<unknown>): Message<C> {
    const options: Parameters<typeof createMessage>[2] = { session: this.session, username: this.username };
    if (parent) options.parent = parent;
    return createMessage(msgType, content, options);
  }

  /**
   * Send a request and resolve with the first `*_reply` on the same channel
   * whose `parent_header.msg_id` matches. Rejects if the socket closes first.
   */
  requestReply<R = JsonObject>(channel: Channel, request: Message<unknown>): Promise<Message<R>> {
    return new Promise<Message<R>>((resolve, reject) => {
      const offMessage = this.onMessage((ch, message) => {
        if (ch !== channel) return;
        if (message.parent_header.msg_id !== request.header.msg_id) return;
        if (!String(message.header.msg_type).endsWith("_reply")) return;
        cleanup();
        resolve(message as Message<R>);
      });
      const offClose = this.onClose((reason) => {
        cleanup();
        reject(reason);
      });
      const cleanup = () => {
        offMessage();
        offClose();
      };
      this.send(channel, request).catch((error: unknown) => {
        cleanup();
        reject(error);
      });
    });
  }

  kernelInfo(): Promise<Message<KernelInfoReplyContent>> {
    return this.requestReply<KernelInfoReplyContent>("shell", this.createMessage("kernel_info_request", {}));
  }

  execute(code: string, options: ExecuteOptions = {}): ExecuteFuture {
    const { metadata, ...contentOverrides } = options;
    const content: ExecuteRequestContent = {
      code,
      silent: false,
      store_history: true,
      user_expressions: {},
      allow_stdin: true,
      stop_on_error: true,
      ...contentOverrides,
    };
    const request = createMessage("execute_request", content, {
      session: this.session,
      username: this.username,
      metadata: metadata ?? {},
    });
    const msgId = request.header.msg_id;
    const iopubListeners = new Set<(message: Message) => void>();

    const done = new Promise<Message<ExecuteReplyContent>>((resolve, reject) => {
      let reply: Message<ExecuteReplyContent> | undefined;
      let idle = false;
      const settle = () => {
        if (reply && idle) {
          cleanup();
          resolve(reply);
        }
      };
      const offMessage = this.onMessage((channel, message) => {
        if (message.parent_header.msg_id !== msgId) return;
        if (channel === "iopub") {
          for (const listener of iopubListeners) listener(message);
          if (message.header.msg_type === "status") {
            const state = (message.content as StatusContent).execution_state;
            if (state === "idle") {
              idle = true;
              settle();
            }
          }
        } else if (channel === "shell" && message.header.msg_type === "execute_reply") {
          reply = message as Message<ExecuteReplyContent>;
          settle();
        }
      });
      const offClose = this.onClose((reason) => {
        cleanup();
        reject(reason);
      });
      const cleanup = () => {
        offMessage();
        offClose();
      };
      this.send("shell", request).catch((error: unknown) => {
        cleanup();
        reject(error);
      });
    });

    return {
      msgId,
      request,
      done,
      onIOPub(listener) {
        iopubListeners.add(listener);
        return () => {
          iopubListeners.delete(listener);
        };
      },
    };
  }

  /**
   * Answer the most recent `input_request` (or the one given). The reply goes
   * on stdin with that request as parent.
   */
  inputReply(value: string, request?: Message<InputRequestContent>): Promise<void> {
    const parent = request ?? this.lastInputRequest;
    if (!parent) {
      return Promise.reject(new Error("inputReply: no pending input_request"));
    }
    if (parent === this.lastInputRequest) {
      this.lastInputRequest = undefined;
    }
    const content: InputReplyContent = { value };
    return this.send("stdin", this.createMessage("input_reply", content, parent));
  }

  /** The pending `input_request`, if the kernel is waiting on stdin. */
  get pendingInputRequest(): Message<InputRequestContent> | undefined {
    return this.lastInputRequest;
  }

  dispose(): void {
    this.listeners.clear();
    if (this.ws.readyState < 2) {
      this.ws.close(1000, "disposed");
    }
  }

  private handleFrame(data: unknown): void {
    let decoded: ChannelMessage;
    if (typeof data === "string") {
      // The server did not negotiate the v1 subprotocol; mirror its format.
      this.sendFormat = "legacy";
      decoded = decodeLegacy(data);
    } else if (data instanceof ArrayBuffer || ArrayBuffer.isView(data)) {
      decoded = decodeV1(data);
    } else {
      // Blob frames only appear if binaryType was not honoured; ignore.
      return;
    }
    const { channel, message } = decoded;
    if (channel === "iopub" && message.header.msg_type === "status") {
      this._status = (message.content as StatusContent).execution_state;
    } else if (channel === "stdin" && message.header.msg_type === "input_request") {
      this.lastInputRequest = message as Message<InputRequestContent>;
    }
    for (const listener of [...this.listeners]) {
      listener(channel, message);
    }
  }
}

function describe(event: unknown): string {
  if (event && typeof event === "object" && "message" in event) {
    return String((event as { message: unknown }).message);
  }
  return "unknown";
}
