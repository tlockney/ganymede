/**
 * The `WidgetManager` seam: the frontend side of the `jupyter.widget` comm
 * target. Interfaces only in this sketch.
 *
 * Two implementations are planned, chosen per deployment profile:
 *
 * 1. `widgets-anywidget`: understands only anywidget models
 *    (`_model_name: "AnyModel"`, `_esm` / `_css` shipped in the model
 *    state). Loads the ESM with a dynamic import of a blob URL, calls its
 *    `render({model, el})`, and keeps model state in sync over comm_msg. No
 *    `@jupyter-widgets/*` dependency. Small, and sufficient for the TS
 *    cdp-kernel and for the iPad profile.
 *
 * 2. `widgets-full`: wraps `@jupyter-widgets/base` and `@jupyter-widgets/
 *    controls`, so ipywidgets from a Python kernel (sliders, HBox, Output,
 *    plus third-party widget packages resolved through `@jupyter-widgets/
 *    html-manager`-style module loading) work. Needed on server and desktop
 *    profiles when Python kernels are expected.
 *
 * Both consume the same comm messages and produce a view for a
 * `application/vnd.jupyter.widget-view+json` output (`{model_id,
 * version_major, version_minor}`), which the widget-view renderer in the
 * registry hands to `renderView`.
 */

import type {
  CommCloseContent,
  CommMsgContent,
  CommOpenContent,
  Message,
} from "../protocol/messages.js";

export const WIDGET_COMM_TARGET = "jupyter.widget";
export const WIDGET_CONTROL_COMM_TARGET = "jupyter.widget.control";

export interface WidgetViewData {
  model_id: string;
  version_major: number;
  version_minor: number;
  [key: string]: unknown;
}

export interface WidgetManager {
  /** Called for every `comm_open` whose `target_name` is a widget target. */
  handleCommOpen(message: Message<CommOpenContent>): Promise<void>;
  handleCommMsg(message: Message<CommMsgContent>): Promise<void>;
  handleCommClose(message: Message<CommCloseContent>): Promise<void>;
  /** Render a view of a known model into `host`. Rejects if the model is unknown. */
  renderView(modelId: string, host: HTMLElement): Promise<void>;
  /** Whether a model with this id exists (e.g. to decide on a placeholder). */
  hasModel(modelId: string): boolean;
  /** Drop all models, e.g. on kernel restart. */
  clear(): void;
}

/**
 * What a widget manager needs from the kernel connection: a way to send
 * comm messages back. Kept narrow so the manager can be tested with a fake.
 */
export interface CommSender {
  sendCommMsg(commId: string, data: Record<string, unknown>, buffers?: ArrayBuffer[]): Promise<void>;
  sendCommClose(commId: string, data?: Record<string, unknown>): Promise<void>;
  sendCommOpen(commId: string, targetName: string, data: Record<string, unknown>): Promise<void>;
}
