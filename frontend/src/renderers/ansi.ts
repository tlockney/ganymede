/**
 * ANSI SGR parsing for stream and error output. Pure; no DOM.
 *
 * `parseAnsi` turns text with escape sequences into styled spans and applies
 * terminal line editing on the way:
 *
 * - `\r` moves the cursor to the start of the current line; following text
 *   overwrites what is there (progress bars).
 * - `\b` deletes the character before the cursor. Terminals only move the
 *   cursor, but Jupyter frontends (JupyterLab's `fixBackspace`) delete, and
 *   that is what notebook users expect, so this follows JupyterLab.
 * - `\r\n` is a plain newline.
 *
 * Supported SGR codes: 0 reset, 1 bold, 2 faint, 3 italic, 4 underline,
 * 7 inverse, 9 strike, 22/23/24/27/29 to undo those, 30-37 / 40-47 colors,
 * 90-97 / 100-107 bright colors, 38;5;n / 48;5;n 256-color, 39 / 49 default.
 * 38;2;r;g;b truecolor is consumed and ignored (a class cannot carry it).
 * Non-SGR CSI sequences are stripped.
 *
 * Classes: `ansi-bold`, `ansi-faint`, `ansi-italic`, `ansi-underline`,
 * `ansi-inverse`, `ansi-strike`, `ansi-fg-<name>`, `ansi-bg-<name>`,
 * `ansi-fg-bright-<name>`, `ansi-bg-bright-<name>`, `ansi-fg-<0..255>`,
 * `ansi-bg-<0..255>`. A stylesheet maps these to colors.
 */

export interface AnsiSpan {
  text: string;
  classes: string[];
}

const COLOR_NAMES = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"] as const;

interface Style {
  bold: boolean;
  faint: boolean;
  italic: boolean;
  underline: boolean;
  inverse: boolean;
  strike: boolean;
  fg: string | null;
  bg: string | null;
}

function freshStyle(): Style {
  return { bold: false, faint: false, italic: false, underline: false, inverse: false, strike: false, fg: null, bg: null };
}

function classKey(style: Style): string {
  const classes: string[] = [];
  if (style.bold) classes.push("ansi-bold");
  if (style.faint) classes.push("ansi-faint");
  if (style.italic) classes.push("ansi-italic");
  if (style.underline) classes.push("ansi-underline");
  if (style.inverse) classes.push("ansi-inverse");
  if (style.strike) classes.push("ansi-strike");
  if (style.fg) classes.push(`ansi-fg-${style.fg}`);
  if (style.bg) classes.push(`ansi-bg-${style.bg}`);
  return classes.join(" ");
}

/** Apply one SGR parameter list (the numbers between `ESC[` and `m`). */
function applySgr(style: Style, params: number[]): void {
  if (params.length === 0) params = [0];
  for (let i = 0; i < params.length; i++) {
    const p = params[i]!;
    if (p === 0) Object.assign(style, freshStyle());
    else if (p === 1) style.bold = true;
    else if (p === 2) style.faint = true;
    else if (p === 3) style.italic = true;
    else if (p === 4) style.underline = true;
    else if (p === 7) style.inverse = true;
    else if (p === 9) style.strike = true;
    else if (p === 22) style.bold = style.faint = false;
    else if (p === 23) style.italic = false;
    else if (p === 24) style.underline = false;
    else if (p === 27) style.inverse = false;
    else if (p === 29) style.strike = false;
    else if (p >= 30 && p <= 37) style.fg = COLOR_NAMES[p - 30]!;
    else if (p >= 40 && p <= 47) style.bg = COLOR_NAMES[p - 40]!;
    else if (p >= 90 && p <= 97) style.fg = `bright-${COLOR_NAMES[p - 90]!}`;
    else if (p >= 100 && p <= 107) style.bg = `bright-${COLOR_NAMES[p - 100]!}`;
    else if (p === 39) style.fg = null;
    else if (p === 49) style.bg = null;
    else if (p === 38 || p === 48) {
      const mode = params[i + 1];
      if (mode === 5 && params[i + 2] !== undefined) {
        const n = Math.max(0, Math.min(255, params[i + 2]!));
        if (p === 38) style.fg = String(n);
        else style.bg = String(n);
        i += 2;
      } else if (mode === 2) {
        // Truecolor: cannot be expressed as a class. Consume r;g;b and ignore.
        i += 4;
      }
    }
    // Anything else is ignored.
  }
}

interface Cell {
  ch: string;
  key: string;
}

// ESC [ <params> <final byte>. Only 'm' (SGR) is interpreted.
const CSI = /\x1b\[([0-9;]*)([\x40-\x7e])/g;

export function parseAnsi(text: string): AnsiSpan[] {
  const style = freshStyle();
  let key = "";
  const lines: Cell[][] = [];
  let line: Cell[] = [];
  let cursor = 0;

  const write = (chunk: string) => {
    for (const ch of chunk) {
      if (ch === "\n") {
        line.push({ ch: "\n", key });
        lines.push(line);
        line = [];
        cursor = 0;
      } else if (ch === "\r") {
        cursor = 0;
      } else if (ch === "\b") {
        if (cursor > 0) {
          cursor -= 1;
          line.splice(cursor, 1);
        }
      } else {
        const cell = { ch, key };
        if (cursor < line.length) line[cursor] = cell;
        else line.push(cell);
        cursor += 1;
      }
    }
  };

  let last = 0;
  for (const match of text.matchAll(CSI)) {
    write(text.slice(last, match.index));
    last = match.index + match[0].length;
    if (match[2] === "m") {
      const params = match[1] === "" ? [] : match[1]!.split(";").map((s) => (s === "" ? 0 : Number(s)));
      applySgr(style, params);
      key = classKey(style);
    }
  }
  write(text.slice(last));
  lines.push(line);

  const spans: AnsiSpan[] = [];
  for (const cells of lines) {
    for (const cell of cells) {
      const current = spans[spans.length - 1];
      if (current && current.classes.join(" ") === cell.key) {
        current.text += cell.ch;
      } else {
        spans.push({ text: cell.ch, classes: cell.key ? cell.key.split(" ") : [] });
      }
    }
  }
  return spans;
}

/** The visible text of a parsed result, with escapes and overwrites applied. */
export function ansiToText(text: string): string {
  return parseAnsi(text)
    .map((span) => span.text)
    .join("");
}
