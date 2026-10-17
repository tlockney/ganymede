/**
 * Jupyter messaging protocol types (version 5.5).
 *
 * Mirrors the Rust `protocol` crate: types and a constructor, no I/O.
 * Reference: https://jupyter-client.readthedocs.io/en/stable/messaging.html
 *
 * Every content interface carries an index signature so that fields this
 * sketch does not know about (for example a capability manifest a kernel
 * adds to `kernel_info_reply`) survive a round trip through these types.
 */

export type JsonObject = Record<string, unknown>;

/** The four client-facing channels. Heartbeat is not exposed over the websocket. */
export type Channel = "shell" | "control" | "iopub" | "stdin";

export const CHANNELS: readonly Channel[] = ["shell", "control", "iopub", "stdin"];

export function isChannel(value: unknown): value is Channel {
  return typeof value === "string" && (CHANNELS as readonly string[]).includes(value);
}

/** Message types this sketch knows about; the header still accepts any string. */
export type KnownMessageType =
  | "execute_request"
  | "execute_reply"
  | "inspect_request"
  | "inspect_reply"
  | "complete_request"
  | "complete_reply"
  | "history_request"
  | "history_reply"
  | "is_complete_request"
  | "is_complete_reply"
  | "kernel_info_request"
  | "kernel_info_reply"
  | "shutdown_request"
  | "shutdown_reply"
  | "interrupt_request"
  | "interrupt_reply"
  | "debug_request"
  | "debug_reply"
  | "status"
  | "stream"
  | "display_data"
  | "update_display_data"
  | "execute_input"
  | "execute_result"
  | "error"
  | "clear_output"
  | "debug_event"
  | "input_request"
  | "input_reply"
  | "comm_open"
  | "comm_msg"
  | "comm_close"
  | "comm_info_request"
  | "comm_info_reply";

/** Known types get completion; unknown strings are still allowed. */
export type MessageType = KnownMessageType | (string & {});

export const PROTOCOL_VERSION = "5.5";

export interface Header {
  msg_id: string;
  session: string;
  username: string;
  date: string;
  msg_type: MessageType;
  version: string;
  /** Protocol 5.5 subshells. Absent or null means the main shell. */
  subshell_id?: string | null;
  [key: string]: unknown;
}

/** `parent_header` is `{}` for messages with no parent, so every field is optional. */
export type ParentHeader = Partial<Header>;

export interface Message<C = JsonObject> {
  header: Header;
  parent_header: ParentHeader;
  metadata: JsonObject;
  content: C;
  /** Raw binary payloads that follow the four JSON parts on the wire. */
  buffers: ArrayBuffer[];
}

// ---------------------------------------------------------------------------
// Shell (request / reply)
// ---------------------------------------------------------------------------

export interface ExecuteRequestContent {
  code: string;
  silent: boolean;
  store_history: boolean;
  user_expressions: Record<string, string>;
  allow_stdin: boolean;
  stop_on_error: boolean;
  [key: string]: unknown;
}

export type ReplyStatus = "ok" | "error" | "abort" | "aborted";

export interface ErrorFields {
  ename: string;
  evalue: string;
  traceback: string[];
}

export interface ExecuteReplyContent extends Partial<ErrorFields> {
  status: ReplyStatus;
  execution_count: number | null;
  payload?: JsonObject[];
  user_expressions?: Record<string, JsonObject>;
  [key: string]: unknown;
}

export interface LanguageInfo {
  name: string;
  version?: string;
  mimetype?: string;
  file_extension?: string;
  pygments_lexer?: string;
  /** A string ("python") or a CodeMirror mode object ({name: "ipython", version: 3}). */
  codemirror_mode?: string | JsonObject;
  nbconvert_exporter?: string;
  [key: string]: unknown;
}

export interface HelpLink {
  text: string;
  url: string;
  [key: string]: unknown;
}

export interface KernelInfoReplyContent {
  status: ReplyStatus;
  protocol_version: string;
  implementation: string;
  implementation_version: string;
  language_info: LanguageInfo;
  banner: string;
  debugger?: boolean;
  help_links?: HelpLink[];
  /** Protocol 5.5: e.g. ["kernel subshells"]. */
  supported_features?: string[];
  /** Anything else, such as a capability manifest, is preserved. */
  [key: string]: unknown;
}

// ---------------------------------------------------------------------------
// IOPub
// ---------------------------------------------------------------------------

export type ExecutionState = "busy" | "idle" | "starting";

export interface StatusContent {
  execution_state: ExecutionState;
  [key: string]: unknown;
}

export type StreamName = "stdout" | "stderr";

export interface StreamContent {
  name: StreamName;
  text: string;
  [key: string]: unknown;
}

/** MIME type to payload. Strings for text types, base64 for images, objects for JSON types. */
export type MimeBundle = Record<string, unknown>;

export interface DisplayTransient {
  display_id?: string;
  [key: string]: unknown;
}

export interface DisplayDataContent {
  data: MimeBundle;
  /** Per-MIME-type metadata, keyed by MIME type. */
  metadata: Record<string, unknown>;
  transient?: DisplayTransient;
  [key: string]: unknown;
}

export type UpdateDisplayDataContent = DisplayDataContent;

export interface ExecuteResultContent extends DisplayDataContent {
  execution_count: number;
}

export interface ErrorContent extends ErrorFields {
  [key: string]: unknown;
}

export interface ExecuteInputContent {
  code: string;
  execution_count: number;
  [key: string]: unknown;
}

export interface ClearOutputContent {
  /** Defer clearing until the next output arrives (avoids flicker). */
  wait: boolean;
  [key: string]: unknown;
}

// ---------------------------------------------------------------------------
// Stdin
// ---------------------------------------------------------------------------

export interface InputRequestContent {
  prompt: string;
  password: boolean;
  [key: string]: unknown;
}

export interface InputReplyContent {
  value: string;
  [key: string]: unknown;
}

// ---------------------------------------------------------------------------
// Comms (used by widgets)
// ---------------------------------------------------------------------------

export interface CommOpenContent {
  comm_id: string;
  target_name: string;
  data: JsonObject;
  target_module?: string;
  [key: string]: unknown;
}

export interface CommMsgContent {
  comm_id: string;
  data: JsonObject;
  [key: string]: unknown;
}

export interface CommCloseContent {
  comm_id: string;
  data: JsonObject;
  [key: string]: unknown;
}

// ---------------------------------------------------------------------------
// Constructor
// ---------------------------------------------------------------------------

export interface CreateMessageOptions {
  /** The client session id; the same for every message this client sends. */
  session: string;
  username?: string;
  /** The message being replied to, if any. Its header becomes `parent_header`. */
  parent?: Message<unknown>;
  metadata?: JsonObject;
  buffers?: ArrayBuffer[];
  subshellId?: string;
}

export function createMessage<C>(
  msgType: MessageType,
  content: C,
  options: CreateMessageOptions,
): Message<C> {
  const header: Header = {
    msg_id: crypto.randomUUID(),
    session: options.session,
    username: options.username ?? "username",
    date: new Date().toISOString(),
    msg_type: msgType,
    version: PROTOCOL_VERSION,
  };
  if (options.subshellId !== undefined) {
    header.subshell_id = options.subshellId;
  }
  return {
    header,
    parent_header: options.parent ? { ...options.parent.header } : {},
    metadata: options.metadata ?? {},
    content,
    buffers: options.buffers ?? [],
  };
}

/** Narrow a message by its `msg_type`. */
export function isMessageType<C>(message: Message<unknown>, msgType: MessageType): message is Message<C> {
  return message.header.msg_type === msgType;
}
