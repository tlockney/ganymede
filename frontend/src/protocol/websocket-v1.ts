/**
 * Codecs for the kernel websocket at `/api/kernels/{id}/channels`.
 *
 * Two formats exist:
 *
 * 1. `v1.kernel.websocket.jupyter.org` subprotocol: one binary frame per
 *    message. Layout, from jupyter_server's `serialize_msg_to_ws_v1`:
 *
 *        offset_number      u64 LE   number of offsets that follow
 *        offsets[0..n)      u64 LE   byte positions (see below)
 *        channel            utf-8    channel name
 *        parts[0..k)        bytes    header, parent_header, metadata,
 *                                    content (JSON), then each binary buffer
 *
 *    offsets[0] is where the channel name starts, which is also the size of
 *    this prefix: 8 * (1 + offset_number). offsets[1] is where the channel
 *    name ends and parts[0] starts. offsets[i+1] is the end of parts[i-1].
 *    The last offset equals the frame length, so offset_number = 2 + k.
 *
 * 2. Legacy (no subprotocol): a JSON text frame with a top-level `channel`
 *    key next to header/parent_header/metadata/content. Messages with
 *    buffers use a separate binary layout in jupyter_server; this sketch
 *    does not implement it and throws instead.
 */

import {
  isChannel,
  type Channel,
  type JsonObject,
  type Message,
} from "./messages.js";

export const WS_SUBPROTOCOL_V1 = "v1.kernel.websocket.jupyter.org";

export interface ChannelMessage<C = JsonObject> {
  channel: Channel;
  message: Message<C>;
}

const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: true });

function toBytes(data: ArrayBuffer | ArrayBufferView): Uint8Array {
  if (ArrayBuffer.isView(data)) {
    return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  }
  return new Uint8Array(data);
}

function copyToArrayBuffer(bytes: Uint8Array): ArrayBuffer {
  const out = new ArrayBuffer(bytes.byteLength);
  new Uint8Array(out).set(bytes);
  return out;
}

export function encodeV1(channel: Channel, message: Message<unknown>): ArrayBuffer {
  const parts: Uint8Array[] = [
    encoder.encode(JSON.stringify(message.header)),
    encoder.encode(JSON.stringify(message.parent_header ?? {})),
    encoder.encode(JSON.stringify(message.metadata ?? {})),
    encoder.encode(JSON.stringify(message.content)),
    ...(message.buffers ?? []).map((b) => toBytes(b)),
  ];
  const channelBytes = encoder.encode(channel);

  const offsetCount = 2 + parts.length;
  const prefixLength = 8 * (1 + offsetCount);
  const offsets: number[] = [prefixLength, prefixLength + channelBytes.byteLength];
  for (const part of parts) {
    offsets.push(offsets[offsets.length - 1]! + part.byteLength);
  }
  const total = offsets[offsets.length - 1]!;

  const frame = new ArrayBuffer(total);
  const view = new DataView(frame);
  const bytes = new Uint8Array(frame);
  view.setBigUint64(0, BigInt(offsetCount), true);
  offsets.forEach((offset, i) => view.setBigUint64(8 * (1 + i), BigInt(offset), true));
  bytes.set(channelBytes, offsets[0]!);
  parts.forEach((part, i) => bytes.set(part, offsets[1 + i]!));
  return frame;
}

export function decodeV1<C = JsonObject>(frame: ArrayBuffer | ArrayBufferView): ChannelMessage<C> {
  const bytes = toBytes(frame);
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (bytes.byteLength < 8) {
    throw new Error("ws v1: frame shorter than the offset count");
  }
  const offsetCount = Number(view.getBigUint64(0, true));
  // channel + four JSON parts means at least 6 offsets.
  if (offsetCount < 6 || 8 * (1 + offsetCount) > bytes.byteLength) {
    throw new Error(`ws v1: invalid offset count ${offsetCount}`);
  }
  const offsets: number[] = [];
  for (let i = 0; i < offsetCount; i++) {
    offsets.push(Number(view.getBigUint64(8 * (1 + i), true)));
  }
  if (offsets[0] !== 8 * (1 + offsetCount)) {
    throw new Error("ws v1: first offset does not match the prefix length");
  }
  for (let i = 1; i < offsets.length; i++) {
    if (offsets[i]! < offsets[i - 1]!) {
      throw new Error("ws v1: offsets are not monotonic");
    }
  }
  if (offsets[offsets.length - 1] !== bytes.byteLength) {
    throw new Error("ws v1: last offset does not equal the frame length");
  }

  const channel = decoder.decode(bytes.subarray(offsets[0], offsets[1]));
  if (!isChannel(channel)) {
    throw new Error(`ws v1: unknown channel ${JSON.stringify(channel)}`);
  }
  const parts: Uint8Array[] = [];
  for (let i = 1; i < offsetCount - 1; i++) {
    parts.push(bytes.subarray(offsets[i], offsets[i + 1]));
  }
  const json = (i: number): JsonObject => JSON.parse(decoder.decode(parts[i])) as JsonObject;
  const message: Message<C> = {
    header: json(0) as Message["header"],
    parent_header: json(1),
    metadata: json(2),
    content: json(3) as C,
    buffers: parts.slice(4).map(copyToArrayBuffer),
  };
  return { channel, message };
}

// ---------------------------------------------------------------------------
// Legacy JSON codec
// ---------------------------------------------------------------------------

export function encodeLegacy(channel: Channel, message: Message<unknown>): string {
  if (message.buffers && message.buffers.length > 0) {
    throw new Error("legacy ws codec: binary buffers are not supported; use the v1 subprotocol");
  }
  return JSON.stringify({
    channel,
    header: message.header,
    parent_header: message.parent_header ?? {},
    metadata: message.metadata ?? {},
    content: message.content,
  });
}

export function decodeLegacy<C = JsonObject>(text: string): ChannelMessage<C> {
  const raw = JSON.parse(text) as Record<string, unknown>;
  const { channel, header, parent_header, metadata, content, buffers } = raw;
  if (!isChannel(channel)) {
    throw new Error(`legacy ws codec: unknown channel ${JSON.stringify(channel)}`);
  }
  if (Array.isArray(buffers) && buffers.length > 0) {
    throw new Error("legacy ws codec: binary buffers are not supported");
  }
  if (typeof header !== "object" || header === null) {
    throw new Error("legacy ws codec: missing header");
  }
  return {
    channel,
    message: {
      header: header as Message["header"],
      parent_header: (parent_header as JsonObject | undefined) ?? {},
      metadata: (metadata as JsonObject | undefined) ?? {},
      content: content as C,
      buffers: [],
    },
  };
}
