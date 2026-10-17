/**
 * Ganymede frontend: public surface.
 *
 * Layout mirrors the Rust crates (docs/architecture.md, Deployment profiles):
 *   protocol/    message types and websocket codecs (no I/O)
 *   connection/  ServerConnection shim, REST client, KernelConnection
 *   renderers/   MIME renderer registry, ANSI, stream buffering, basic renderers
 *   widgets/     WidgetManager seam (anywidget-only and full implementations to come)
 *   editor/      not started (CodeMirror 6)
 *   app/         not started
 */

export * from "./protocol/messages.js";
export * from "./protocol/websocket-v1.js";
export * from "./connection/server-connection.js";
export * from "./connection/kernel-connection.js";
export * from "./renderers/registry.js";
export * from "./renderers/ansi.js";
export * from "./renderers/stream.js";
export * from "./renderers/basic.js";
export * from "./widgets/manager.js";
