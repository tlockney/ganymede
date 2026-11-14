// Copies the cell API reference pages into docs/cell-api/, which VitePress
// renders under /docs/cell-api/ (#596, #607). The pages are generated from
// TSDoc in their own repositories, so their main is the source and the
// copy is not committed:
//
//   cdp-kernel        docs/jupyter-reference.md  -> cell-api/jupyter.md
//                     docs/widgets-reference.md  -> cell-api/widgets.md
//                     docs/images/widgets/       -> cell-api/images/widgets/
//   ganymede-ui       packages/device/docs/reference.md -> cell-api/device.md
//
// A cdp-kernel page that names a notebook (<!-- notebook: … -->) shows,
// after each of its ```ts cell blocks, the outputs that cell saved there,
// as cdp-kernel.org does: images go in cell-api/images/, and widgets saved
// with their state run on the page (theme/widgets.ts).
//
// Links between the pages follow the renames. A link to a cdp-kernel guide
// goes to cdp-kernel.org; any other relative link goes to its repository on
// GitHub, so a new link upstream can't break this site's build.
//
//   npm run fetch-cell-api
//   CDP_KERNEL_DIR=~/src/cdp-kernel GANYMEDE_UI_DIR=~/src/ganymede-ui npm run fetch-cell-api
import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { output, text, widgetsOf } from "./fetch-examples.mjs";

const docs = new URL("../", import.meta.url).pathname;
const out = join(docs, "cell-api");

const sources = [
  {
    repo: "tlockney/cdp-kernel",
    env: "CDP_KERNEL_DIR",
    docs: "docs/",
    // Its docs site: guides link there rather than to GitHub.
    site: "https://cdp-kernel.org/docs/",
    files: { "docs/jupyter-reference.md": "jupyter.md", "docs/widgets-reference.md": "widgets.md" },
    dirs: { "docs/images/widgets": "images/widgets" },
    // The notebooks the pages name, read for their outputs, not copied.
    notebooks: "docs/public/notebooks",
  },
  {
    // The device package, in ganymede-ui since ganymede-ui#560.
    repo: "tlockney/ganymede-ui",
    env: "GANYMEDE_UI_DIR",
    docs: "packages/device/docs/",
    files: { "packages/device/docs/reference.md": "device.md" },
    dirs: {},
  },
];

/** A checkout of `repo`'s main with only `paths`, or the local one `env` names. */
function checkout(source, tmp) {
  const local = process.env[source.env];
  if (local) return local;
  const dir = join(tmp, source.repo.split("/")[1]);
  const url = `https://github.com/${source.repo}.git`;
  execFileSync("git", ["clone", "--quiet", "--depth", "1", "--filter=blob:none", "--sparse", url, dir]);
  const dirs = [...Object.keys(source.dirs), ...(source.notebooks ? [source.notebooks] : [])];
  execFileSync("git", ["-C", dir, "sparse-checkout", "set", "--no-cone", ...Object.keys(source.files).map((f) => `/${f}`), ...dirs.map((d) => `/${d}/`)]);
  const rev = execFileSync("git", ["-C", dir, "rev-parse", "--short", "HEAD"], { encoding: "utf8" }).trim();
  console.log(`${source.repo} ${rev}`);
  return dir;
}

/**
 * `markdown` from `source.docs`, with its relative links pointed where
 * they resolve on this site. Fenced code is left as it is: `f(x)` after a
 * `]` there is code, not a link.
 */
export function relink(markdown, source) {
  return markdown
    .split(/(^ *(?:`{3,}|~{3,})[^\n]*\n[\s\S]*?^ *(?:`{3,}|~{3,})[ \t]*$)/m)
    .map((part, i) => (i % 2 === 1 ? part : relinkProse(part, source)))
    .join("");
}

function relinkProse(markdown, source) {
  const renames = new Map(
    Object.entries(source.files).map(([from, to]) => {
      if (!from.startsWith(source.docs)) throw new Error(`${from} is not under ${source.docs}`);
      return [from.slice(source.docs.length), to];
    }),
  );
  return markdown.replace(/\]\(([^)\s]+)\)/g, (link, href) => {
    // Pages of this site, linked absolutely so they work on GitHub too.
    const own = href.match(/^https:\/\/ganymede-server\.com\/docs\/([\w/-]+?)(#.*)?$/);
    if (own) return `](../${own[1]}.md${own[2] ?? ""})`;
    // Absolute URLs, in-page anchors and images (which are copied) stay.
    if (/^[a-z][a-z0-9+.-]*:|^#|^\/|^images\//i.test(href)) return link;
    const [target, hash = ""] = href.split(/(?=#)/);
    if (renames.has(target)) return `](${renames.get(target)}${hash})`;
    if (source.site && /^[\w-]+\.md$/.test(target)) return `](${source.site}${target.replace(/\.md$/, "")}${hash})`;
    return `](https://github.com/${source.repo}/blob/main/${source.docs}${target}${hash})`;
  });
}

export { sources };

const markerRE = /^<!--\s*notebook:\s*(\S+\.ipynb)\s*-->[ \t]*\n?/m;
// As cdp-kernel's docs build compares them: trailing whitespace doesn't count.
const normalize = (s) => s.replace(/\s+$/, "");

/** The notebook page `markdown` names, relative to the page, if any. */
export const notebookOf = (markdown) => markdown.match(markerRE)?.[1];

/**
 * `markdown` with the outputs of each ```ts cell block from `nb` (the
 * parsed notebook the page names) after the block, and the marker and the
 * blocks' `cell` taken out. Images go in `files`, keyed by their path under
 * cell-api/. A block the notebook doesn't have as written fails, as it does
 * on cdp-kernel.org, so a stale notebook can't show the wrong output.
 */
export function withOutputs(markdown, nb, slug, files) {
  const widgets = widgetsOf(nb);
  const cells = nb.cells.filter((c) => c.cell_type === "code");
  const lines = markdown.replace(markerRE, "").split("\n");
  const out = [];
  let next = 0;
  for (let i = 0; i < lines.length; i++) {
    const open = lines[i].match(/^( *)(`{3,}|~{3,})(\S*)(.*)$/);
    if (!open) {
      out.push(lines[i]);
      continue;
    }
    const [, indent, fence, lang, rest] = open;
    // Closed by a fence of the same character at least as long, with
    // nothing after it (CommonMark).
    const close = new RegExp(`^ *${fence[0] === "`" ? "`" : "~"}{${fence.length},}\\s*$`);
    let j = i + 1;
    while (j < lines.length && !close.test(lines[j])) j++;
    const body = lines.slice(i + 1, j);
    // Indented blocks aren't cells, as in cdp-kernel's runner.
    if (indent || !/^\s+cell\b/.test(rest)) {
      out.push(...lines.slice(i, j + 1));
      i = j;
      continue;
    }
    const source = normalize(body.map((l) => l.slice(indent.length)).join("\n"));
    // In page order, so two identical blocks get their own cells' outputs.
    const at = cells.findIndex((c, k) => k >= next && normalize(text(c.source)) === source);
    const cell = cells[at];
    if (at >= 0) next = at + 1;
    if (!cell) throw new Error(`${slug}: a \`cell\` block isn't in its notebook as written:\n${source}`);
    out.push(`${indent}${fence}${lang}${rest.replace(/^\s+cell\b/, "")}`, ...body, lines[j] ?? fence);
    for (const o of cell.outputs ?? []) {
      const md = output(o, slug, files, widgets);
      if (md) out.push("", ...md.split("\n"));
    }
    // An HTML output runs to the next blank line, so prose right after the
    // block would join it.
    if ((cell.outputs ?? []).length && lines[j + 1]?.trim()) out.push("");
    i = j;
  }
  return out.join("\n");
}

// Run as a script, not imported by the test. Not `import.meta.main`, which
// Node before 22.18 lacks.
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const tmp = mkdtempSync(join(tmpdir(), "cell-api-"));
  try {
    rmSync(out, { recursive: true, force: true });
    mkdirSync(out, { recursive: true });
    for (const source of sources) {
      const dir = checkout(source, tmp);
      for (const [from, to] of Object.entries(source.files)) {
        const file = join(dir, from);
        if (!existsSync(file)) throw new Error(`${source.repo} has no ${from}`);
        // Relinked before the outputs go in, so widget code in them is left alone.
        let markdown = relink(readFileSync(file, "utf8"), source);
        const notebook = notebookOf(markdown);
        const files = new Map();
        if (notebook) {
          const nbFile = join(dirname(file), notebook);
          if (!existsSync(nbFile)) throw new Error(`${source.repo} has no ${notebook} beside ${from}`);
          markdown = withOutputs(markdown, JSON.parse(readFileSync(nbFile, "utf8")), to.replace(/\.md$/, ""), files);
        }
        writeFileSync(join(out, to), markdown);
        for (const [name, body] of files) {
          mkdirSync(dirname(join(out, name)), { recursive: true });
          writeFileSync(join(out, name), body);
        }
        console.log(`  ${to}${files.size ? ` (${files.size} images)` : ""}`);
      }
      for (const [from, to] of Object.entries(source.dirs)) {
        if (!existsSync(join(dir, from))) continue;
        cpSync(join(dir, from), join(out, to), { recursive: true });
        console.log(`  ${to}/`);
      }
    }
  } finally {
    rmSync(tmp, { recursive: true, force: true });
  }
}
