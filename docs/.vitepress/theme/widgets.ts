// Runs widgets saved in a notebook (docs/scripts/fetch-examples.mjs marks
// them .nb-widget) on the page: the widget's own anywidget module renders
// into the output, against a model that holds the saved state. No kernel is
// behind it, so the model only changes in the browser; messages to the
// kernel go nowhere. The same as cdp-kernel.org's (its
// docs/.vitepress/theme/widgets.ts); keep the two in step.

type Callback = (...args: unknown[]) => void;

interface Saved {
  state: Record<string, unknown>;
  buffers?: { path: (string | number)[]; data: string; encoding: "base64" | "hex" }[];
  /** Custom messages the kernel sent the views, replayed after render. */
  messages?: { content: unknown; buffers?: string[] }[];
}

/** Each widget's stylesheet (anywidget's `_css`), added to the page once. */
const styles = new Set<string>();

function decode(data: string, encoding: "base64" | "hex"): DataView {
  const bytes = encoding === "hex"
    ? new Uint8Array((data.match(/../g) ?? []).map((h) => parseInt(h, 16)))
    : Uint8Array.from(atob(data), (c) => c.charCodeAt(0));
  return new DataView(bytes.buffer);
}

/** The model API anywidget gives a view (get, set, on, save_changes, …). */
function localModel(saved: Saved) {
  const state: Record<string, unknown> = structuredClone(saved.state);
  // Binary values are saved apart from the state, at a path into it.
  for (const { path, data, encoding } of saved.buffers ?? []) {
    let target = state as Record<string | number, unknown>;
    for (const key of path.slice(0, -1)) target = target[key] as Record<string | number, unknown>;
    target[path[path.length - 1]] = decode(data, encoding);
  }
  const listeners = new Map<string, Set<Callback>>();
  const emit = (event: string, ...args: unknown[]) => listeners.get(event)?.forEach((cb) => cb(...args));
  const model = {
    get: (key: string) => state[key],
    // As in JupyterLab's (Backbone) models: set takes a key and value or an
    // object of them, and change events fire at once. save_changes would
    // send them to the kernel, so here it does nothing.
    set(key: string | Record<string, unknown>, value?: unknown) {
      const changes = typeof key === "string" ? { [key]: value } : key;
      const keys = Object.keys(changes).filter((k) => state[k] !== changes[k]);
      Object.assign(state, changes);
      for (const k of keys) emit(`change:${k}`, model, state[k]);
      if (keys.length) emit("change", model);
    },
    save_changes() {},
    on(event: string, cb: Callback) {
      if (!listeners.has(event)) listeners.set(event, new Set());
      listeners.get(event)!.add(cb);
    },
    off(event?: string, cb?: Callback) {
      if (!event) listeners.clear();
      else if (!cb) listeners.delete(event);
      else listeners.get(event)?.delete(cb);
    },
    send() {},
    widget_manager: { get_model: () => Promise.reject(new Error("no other models on this page")) },
    emit,
  };
  return model;
}

async function mount(el: HTMLElement) {
  const saved = JSON.parse(el.dataset.model!) as Saved;
  const url = URL.createObjectURL(new Blob([saved.state._esm as string], { type: "text/javascript" }));
  try {
    const mod = await import(/* @vite-ignore */ url);
    // As anywidget does: a default-exported function is a factory, called with
    // no arguments, that returns { initialize, render }.
    const widget = typeof mod.default === "function" ? await mod.default() : (mod.default ?? mod);
    const css = saved.state._css;
    if (typeof css === "string" && css && !styles.has(css)) {
      styles.add(css);
      const style = document.createElement("style");
      style.textContent = css;
      document.head.append(style);
    }
    const model = localModel(saved);
    await widget.initialize?.({ model });
    await widget.render?.({ model, el });
    // What the kernel said to the views in reply to the docs build's
    // scripted clicks (a dice roll), so the view shows it as it did.
    for (const { content, buffers } of saved.messages ?? []) {
      model.emit("msg:custom", content, (buffers ?? []).map((b) => decode(b, "base64")));
    }
  } catch (error) {
    el.textContent = `This widget couldn't start on this page: ${error}`;
    el.classList.add("nb-widget-failed");
  } finally {
    URL.revokeObjectURL(url);
  }
}

export function runWidgets() {
  for (const el of document.querySelectorAll<HTMLElement>(".nb-widget:not([data-mounted])")) {
    el.dataset.mounted = "";
    void mount(el);
  }
}
