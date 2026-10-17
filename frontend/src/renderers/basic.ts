/**
 * Built-in renderers for the common MIME types. These touch the DOM and
 * are not unit tested; the pure parts they use (ansi.ts) are.
 *
 * Trust: `text/html` and `image/svg+xml` can carry scripts. They are only
 * put into the live document when `context.trusted` is true. Untrusted HTML
 * is shown as text; untrusted SVG is shown through an `<img>`, which does
 * not run scripts.
 *
 * TODO(sanitize): untrusted HTML should be sanitized rather than shown as
 * text. DOMPurify is the likely choice (JupyterLab uses it via
 * `@jupyterlab/apputils` ISanitizer). Not added because the sketch has no
 * runtime dependencies yet.
 * TODO(isolated): `text/html` with `metadata.isolated === true` should go
 * into a sandboxed iframe even when trusted.
 */

import { parseAnsi } from "./ansi.js";
import { defaultRank, type MimeRenderer, type RenderContext } from "./registry.js";
import type { JsonObject } from "../protocol/messages.js";

/** nbformat allows multiline strings as arrays of lines. */
export function asString(data: unknown): string {
  if (typeof data === "string") return data;
  if (Array.isArray(data)) return data.map((d) => String(d)).join("");
  if (data === undefined || data === null) return "";
  return JSON.stringify(data, null, 2);
}

/** Append ANSI-styled spans into an element (used for streams and errors too). */
export function appendAnsi(host: HTMLElement, text: string): void {
  const doc = host.ownerDocument;
  for (const span of parseAnsi(text)) {
    if (span.classes.length === 0) {
      host.append(doc.createTextNode(span.text));
    } else {
      const el = doc.createElement("span");
      el.className = span.classes.join(" ");
      el.textContent = span.text;
      host.append(el);
    }
  }
}

function applySize(el: HTMLElement, metadata: JsonObject): void {
  if (typeof metadata["width"] === "number") el.style.width = `${metadata["width"]}px`;
  if (typeof metadata["height"] === "number") el.style.height = `${metadata["height"]}px`;
}

export const textPlainRenderer: MimeRenderer = {
  mimeTypes: ["text/plain"],
  rank: defaultRank("text/plain"),
  render(data, _metadata, host) {
    const pre = host.ownerDocument.createElement("pre");
    pre.className = "jp-output-text";
    appendAnsi(pre, asString(data));
    host.replaceChildren(pre);
  },
};

function imageRenderer(mime: "image/png" | "image/jpeg"): MimeRenderer {
  return {
    mimeTypes: [mime],
    rank: defaultRank(mime),
    render(data, metadata, host) {
      const img = host.ownerDocument.createElement("img");
      img.src = `data:${mime};base64,${asString(data).replace(/\s+/g, "")}`;
      applySize(img, metadata);
      host.replaceChildren(img);
    },
  };
}

export const imagePngRenderer = imageRenderer("image/png");
export const imageJpegRenderer = imageRenderer("image/jpeg");

export const svgRenderer: MimeRenderer = {
  mimeTypes: ["image/svg+xml"],
  rank: defaultRank("image/svg+xml"),
  render(data, metadata, host, context: RenderContext = { trusted: false }) {
    const svg = asString(data);
    if (context.trusted) {
      // TODO(sanitize): even trusted SVG could go through DOMPurify in SVG profile.
      host.innerHTML = svg;
      return;
    }
    const img = host.ownerDocument.createElement("img");
    img.src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
    applySize(img, metadata);
    host.replaceChildren(img);
  },
};

export const htmlRenderer: MimeRenderer = {
  mimeTypes: ["text/html"],
  rank: defaultRank("text/html"),
  render(data, _metadata, host, context: RenderContext = { trusted: false }) {
    const html = asString(data);
    if (context.trusted) {
      // Note: scripts inserted via innerHTML do not execute; JupyterLab
      // re-creates <script> nodes to run them. Deliberately not done here.
      host.innerHTML = html;
      return;
    }
    // TODO(sanitize): run through DOMPurify and insert instead of escaping.
    const pre = host.ownerDocument.createElement("pre");
    pre.className = "jp-output-untrusted-html";
    pre.textContent = html;
    host.replaceChildren(pre);
  },
};

export const basicRenderers: readonly MimeRenderer[] = [
  textPlainRenderer,
  imagePngRenderer,
  imageJpegRenderer,
  svgRenderer,
  htmlRenderer,
];
