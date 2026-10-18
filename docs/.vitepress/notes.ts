// The development log is one file per entry in docs/notes/, so PRs open at
// the same time don't conflict on a shared file. The site still shows it as
// one page: notes.md's own text followed by every entry, archive.md first and
// then the dated files in name order. config.mts applies this both to the
// page (through a Vite plugin that runs before VitePress's Markdown one) and
// to the local search index, which reads notes.md from disk.
import fs from "node:fs";
import path from "node:path";
import type { Plugin } from "vite";

const entryName = /^\d{4}-\d{2}-\d{2}-[a-z0-9-]+\.md$/;

export function entryFiles(docsDir: string): string[] {
  const dir = path.join(docsDir, "notes");
  const names = fs.readdirSync(dir).filter((n) => n.endsWith(".md")).sort();
  // A misnamed file would otherwise vanish from the page without a trace.
  const bad = names.filter((n) => n !== "archive.md" && !entryName.test(n));
  if (bad.length) {
    throw new Error(`docs/notes/: name entries YYYY-MM-DD-<topic>.md: ${bad.join(", ")}`);
  }
  const dated = names.filter((n) => n !== "archive.md");
  const ordered = names.includes("archive.md") ? ["archive.md", ...dated] : dated;
  return ordered.map((n) => path.join(dir, n));
}

// Entries link relative to docs/notes/ so they work on GitHub; on the page
// they sit in docs/, so rebase each relative link target. Fenced code is left
// alone.
function rebase(src: string, fromDir: string, toDir: string): string {
  let fenced = false;
  return src
    .split("\n")
    .map((line) => {
      if (/^\s*(```|~~~)/.test(line)) fenced = !fenced;
      if (fenced) return line;
      return line.replace(/\]\(([^)\s]+)/g, (m, href: string) => {
        if (/^[a-z][a-z0-9+.-]*:|^#|^\//i.test(href)) return m;
        const rel = path.relative(toDir, path.resolve(fromDir, href)).split(path.sep).join("/");
        return `](${rel}${href.endsWith("/") ? "/" : ""}`;
      });
    })
    .join("\n");
}

export function withEntries(src: string, docsDir: string): string {
  const parts = entryFiles(docsDir).map((f) => rebase(fs.readFileSync(f, "utf-8").trim(), path.dirname(f), docsDir));
  return [src.trimEnd(), ...parts].join("\n\n") + "\n";
}

export function notesPlugin(docsDir: string): Plugin {
  const page = path.join(docsDir, "notes.md");
  return {
    name: "ganymede:notes",
    enforce: "pre",
    transform(code, id) {
      if (id.split("?")[0] !== page) return;
      for (const f of entryFiles(docsDir)) this.addWatchFile(f);
      return withEntries(code, docsDir);
    },
  };
}
