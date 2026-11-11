// Renders the apps' example notebooks as pages under docs/examples/, which
// VitePress publishes under /docs/examples/ (#689). The notebooks live in
// ganymede-ui's examples/, saved with their outputs, so its main is the
// source and the pages are not committed:
//
//   ganymede-ui  examples/01 Start Here.ipynb -> examples/01-start-here.md
//                (each output image)          -> examples/images/01-start-here-<n>.<ext>
//
// pages.json lists the pages and their titles, for the sidebar.
//
// A page shows each cell and what it showed when ganymede-ui last ran the
// notebooks. Markdown, text, JSON, errors, SVG and bitmaps render as they
// are. HTML renders in place, or, when it has a script, in a sandboxed
// iframe. A widget needs a kernel, so the page says so instead.
//
//   npm run fetch-examples
//   GANYMEDE_UI_DIR=~/src/ganymede-ui npm run fetch-examples
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const docs = new URL("../", import.meta.url).pathname;
const out = join(docs, "examples");
const repo = "tlockney/ganymede-ui";

/** "01 Start Here.ipynb" -> "01-start-here". */
export function slugOf(file) {
  return file
    .replace(/\.ipynb$/, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "");
}

const text = (v) => (Array.isArray(v) ? v.join("") : (v ?? ""));
// Terminal colours in tracebacks.
const plain = (s) => s.replace(/\x1b\[[0-9;]*m/g, "");

/** A fence long enough that `body` can't close it. */
function fence(body, lang) {
  const longest = Math.max(2, ...(body.match(/`{3,}/g) ?? []).map((f) => f.length));
  const f = "`".repeat(longest + 1);
  return `${f}${lang}\n${body.replace(/\n$/, "")}\n${f}`;
}

// Markdown that Vue mustn't read as a template (`{{ }}`): VitePress
// renders Markdown inside a block with blank lines around it.
const raw = (md) => `<div v-pre>\n\n${md.trim()}\n\n</div>`;

// Newlines too: a blank line would end the HTML block mid-attribute.
const attr = (s) =>
  s.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/\n/g, "&#10;");

/**
 * One output as Markdown. Images go in `files`, keyed by their path under
 * examples/, and the Markdown links them.
 */
function output(o, slug, files) {
  if (o.output_type === "stream") return fence(plain(text(o.text)), "text");
  if (o.output_type === "error") {
    const trace = (o.traceback ?? []).map(plain).join("\n");
    return fence(trace || `${o.ename}: ${o.evalue}`, "text");
  }
  const data = o.data ?? {};
  if (data["application/vnd.jupyter.widget-view+json"]) {
    return '::: info Widget\nAn interactive widget, which needs a running kernel. Open the notebook in the app to use it.\n:::';
  }
  for (const [mime, ext] of [["image/svg+xml", "svg"], ["image/png", "png"], ["image/jpeg", "jpg"]]) {
    if (!data[mime]) continue;
    const name = `images/${slug}-${files.size + 1}.${ext}`;
    files.set(name, ext === "svg" ? text(data[mime]) : Buffer.from(text(data[mime]).replace(/\s/g, ""), "base64"));
    return `<p class="nb-output"><img src="./${name}" alt="Output of the cell above (${mime})"></p>`;
  }
  if (data["text/html"]) {
    const html = text(data["text/html"]);
    // Anything that runs, styles the page, loads something, or keeps its
    // whitespace goes in a sandboxed frame with an opaque origin; the rest
    // is plain markup, shown in place.
    if (/<(script|style|link|iframe|object|embed|pre|textarea)\b|\son\w+\s*=/i.test(html)) {
      return `<iframe class="nb-output" title="Output of the cell above" sandbox="allow-scripts" loading="lazy" srcdoc="${attr(html)}"></iframe>`;
    }
    // A blank line would end the HTML block and turn the rest into Markdown.
    return `<div class="nb-output" v-pre>\n${html.trim().replace(/\n\s*\n/g, "\n")}\n</div>`;
  }
  if (data["text/markdown"]) return raw(text(data["text/markdown"]));
  if (data["application/json"]) return fence(JSON.stringify(data["application/json"], null, 2), "json");
  if (data["text/plain"]) return fence(text(data["text/plain"]), "text");
  return "";
}

/**
 * The page for notebook `nb` (parsed .ipynb), named `file` in ganymede-ui's
 * examples/. Returns the Markdown and the images it links.
 */
export function render(nb, file) {
  const slug = slugOf(file);
  const files = new Map();
  const lang = nb.metadata?.language_info?.name === "typescript" ? "ts" : (nb.metadata?.language_info?.name ?? "");
  const source = `https://github.com/${repo}/blob/main/examples/${encodeURIComponent(file)}`;
  const parts = [];
  for (const [i, cell] of nb.cells.entries()) {
    const src = text(cell.source);
    if (cell.cell_type === "markdown") {
      // Right after the notebook's title, where it came from.
      const title = i === 0 ? src.match(/^# .*\n?/m) : null;
      if (title) {
        const at = title.index + title[0].length;
        parts.push(raw(src.slice(0, at)));
        parts.push(
          `> [!NOTE]\n> One of the notebooks the apps' **Open Example Notebooks** copies, shown with the outputs it saved when ganymede-ui last ran it. [The notebook](${source}) is in ganymede-ui.`,
        );
        if (src.slice(at).trim()) parts.push(raw(src.slice(at)));
      } else {
        parts.push(raw(src));
      }
    } else if (cell.cell_type === "code") {
      if (src.trim()) parts.push(fence(src, lang));
      for (const o of cell.outputs ?? []) {
        const md = output(o, slug, files);
        if (md) parts.push(md);
      }
    }
  }
  return { markdown: `${parts.join("\n\n")}\n`, files };
}

/** A notebook's title: its first heading, else its file name. */
export function titleOf(nb, file) {
  for (const cell of nb.cells) {
    const m = cell.cell_type === "markdown" && text(cell.source).match(/^# (.+)$/m);
    if (m) return m[1].trim();
  }
  return file.replace(/\.ipynb$/, "").replace(/^\d+\s+/, "");
}

/** A checkout of ganymede-ui's examples/, or the local one GANYMEDE_UI_DIR names. */
function checkout(tmp) {
  if (process.env.GANYMEDE_UI_DIR) return process.env.GANYMEDE_UI_DIR;
  const dir = join(tmp, "ganymede-ui");
  execFileSync("git", ["clone", "--quiet", "--depth", "1", "--filter=blob:none", "--sparse", `https://github.com/${repo}.git`, dir]);
  execFileSync("git", ["-C", dir, "sparse-checkout", "set", "--no-cone", "/examples/*.ipynb"]);
  const rev = execFileSync("git", ["-C", dir, "rev-parse", "--short", "HEAD"], { encoding: "utf8" }).trim();
  console.log(`${repo} ${rev}`);
  return dir;
}

// Run as a script, not imported by the test.
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const tmp = mkdtempSync(join(tmpdir(), "examples-"));
  try {
    const dir = join(checkout(tmp), "examples");
    rmSync(out, { recursive: true, force: true });
    mkdirSync(join(out, "images"), { recursive: true });
    const notebooks = readdirSync(dir).filter((f) => f.endsWith(".ipynb")).sort();
    if (notebooks.length === 0) throw new Error(`${repo} has no examples/*.ipynb`);
    // The pages, in sidebar order, for config.mts.
    const pages = [];
    for (const file of notebooks) {
      const nb = JSON.parse(readFileSync(join(dir, file), "utf8"));
      const { markdown, files } = render(nb, file);
      writeFileSync(join(out, `${slugOf(file)}.md`), markdown);
      for (const [name, body] of files) writeFileSync(join(out, name), body);
      pages.push({ text: titleOf(nb, file), link: `/examples/${slugOf(file)}` });
      console.log(`  ${slugOf(file)}.md${files.size ? ` (${files.size} images)` : ""}`);
    }
    writeFileSync(join(out, "pages.json"), `${JSON.stringify(pages, null, 2)}\n`);
  } finally {
    rmSync(tmp, { recursive: true, force: true });
  }
}
