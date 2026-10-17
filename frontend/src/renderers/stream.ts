/**
 * Stream output accumulation. Pure; no DOM.
 *
 * Kernels send `stream` messages in small chunks, often one per write.
 * `StreamBuffer` merges consecutive chunks of the same stream into one
 * entry and keeps a plain-text view with `\r` / `\b` applied (the same
 * semantics as `parseAnsi`, see ansi.ts), so a progress bar written with
 * `\r` across many chunks occupies one line.
 *
 * Two views are kept per entry:
 * - `raw`: the chunks concatenated, escapes and control characters intact.
 *   Feed this to `parseAnsi` for rendering; it handles `\r`/`\b` with style
 *   awareness, and it is what belongs in the notebook file.
 * - `text`: control characters applied, escapes left in place. For plain
 *   consumers (search, copy, tests). Escape sequences count as characters
 *   here, so overwrites of colored text can be off by the escape length; the
 *   rendered view does not have that problem.
 */

import type { StreamName } from "../protocol/messages.js";

export type { StreamName };

/** Incremental `\r` / `\b` / `\n` cursor state for one text stream. */
class LineState {
  done = "";
  line: string[] = [];
  cursor = 0;

  feed(chunk: string): void {
    for (const ch of chunk) {
      if (ch === "\n") {
        this.done += this.line.join("") + "\n";
        this.line = [];
        this.cursor = 0;
      } else if (ch === "\r") {
        this.cursor = 0;
      } else if (ch === "\b") {
        if (this.cursor > 0) {
          this.cursor -= 1;
          this.line.splice(this.cursor, 1);
        }
      } else {
        if (this.cursor < this.line.length) this.line[this.cursor] = ch;
        else this.line.push(ch);
        this.cursor += 1;
      }
    }
  }

  get text(): string {
    return this.done + this.line.join("");
  }
}

/** Apply `\r` and `\b` to plain text in one pass. */
export function applyControlChars(text: string): string {
  const state = new LineState();
  state.feed(text);
  return state.text;
}

export interface StreamEntry {
  name: StreamName;
  /** Chunks concatenated as received. */
  raw: string;
  /** `raw` with `\r` and `\b` applied. */
  text: string;
}

export class StreamBuffer {
  private readonly items: { name: StreamName; raw: string; state: LineState }[] = [];

  /** Append a chunk, merging into the last entry when it is the same stream. */
  append(name: StreamName, chunk: string): StreamEntry {
    let last = this.items[this.items.length - 1];
    if (!last || last.name !== name) {
      last = { name, raw: "", state: new LineState() };
      this.items.push(last);
    }
    last.raw += chunk;
    last.state.feed(chunk);
    return { name: last.name, raw: last.raw, text: last.state.text };
  }

  get entries(): StreamEntry[] {
    return this.items.map((item) => ({ name: item.name, raw: item.raw, text: item.state.text }));
  }

  get length(): number {
    return this.items.length;
  }

  /** Joined `text` of every entry, or of one stream. */
  text(name?: StreamName): string {
    return this.items
      .filter((item) => name === undefined || item.name === name)
      .map((item) => item.state.text)
      .join("");
  }

  clear(): void {
    this.items.length = 0;
  }
}
