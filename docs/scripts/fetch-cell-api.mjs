// Copies the cell API reference pages into docs/cell-api/, which VitePress
// renders under /docs/cell-api/ (#596, #607). The pages are generated from
// TSDoc in their own repositories, so their main is the source and the
// copy is not committed:
//
//   cdp-kernel        docs/jupyter-reference.md  -> cell-api/jupyter.md
//                     docs/widgets-reference.md  -> cell-api/widgets.md
//                     docs/images/widgets/       -> cell-api/images/widgets/
//   ganymede-device   docs/reference.md          -> cell-api/device.md
//
// Links between the pages follow the renames. A link to a cdp-kernel guide
// goes to cdp-kernel.org; any other relative link goes to its repository on
// GitHub, so a new link upstream can't break this site's build.
//
//   npm run fetch-cell-api
//   CDP_KERNEL_DIR=~/src/cdp-kernel GANYMEDE_DEVICE_DIR=~/src/ganymede-device npm run fetch-cell-api
import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const docs = new URL("../", import.meta.url).pathname;
const out = join(docs, "cell-api");

const sources = [
  {
    repo: "tlockney/cdp-kernel",
    env: "CDP_KERNEL_DIR",
    // Its docs site: guides link there rather than to GitHub.
    site: "https://cdp-kernel.org/docs/",
    files: { "docs/jupyter-reference.md": "jupyter.md", "docs/widgets-reference.md": "widgets.md" },
    dirs: { "docs/images/widgets": "images/widgets" },
  },
  {
    repo: "tlockney/ganymede-device",
    env: "GANYMEDE_DEVICE_DIR",
    files: { "docs/reference.md": "device.md" },
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
  execFileSync("git", ["-C", dir, "sparse-checkout", "set", "--no-cone", ...Object.keys(source.files).map((f) => `/${f}`), ...Object.keys(source.dirs).map((d) => `/${d}/`)]);
  const rev = execFileSync("git", ["-C", dir, "rev-parse", "--short", "HEAD"], { encoding: "utf8" }).trim();
  console.log(`${source.repo} ${rev}`);
  return dir;
}

/**
 * `markdown` from `source`'s docs/, with its relative links pointed where
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
  const renames = new Map(Object.entries(source.files).map(([from, to]) => [from.replace(/^docs\//, ""), to]));
  return markdown.replace(/\]\(([^)\s]+)\)/g, (link, href) => {
    // Pages of this site, linked absolutely so they work on GitHub too.
    const own = href.match(/^https:\/\/ganymede-server\.com\/docs\/([\w/-]+?)(#.*)?$/);
    if (own) return `](../${own[1]}.md${own[2] ?? ""})`;
    // Absolute URLs, in-page anchors and images (which are copied) stay.
    if (/^[a-z][a-z0-9+.-]*:|^#|^\/|^images\//i.test(href)) return link;
    const [target, hash = ""] = href.split(/(?=#)/);
    if (renames.has(target)) return `](${renames.get(target)}${hash})`;
    if (source.site && /^[\w-]+\.md$/.test(target)) return `](${source.site}${target.replace(/\.md$/, "")}${hash})`;
    return `](https://github.com/${source.repo}/blob/main/docs/${target}${hash})`;
  });
}

export { sources };

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
        writeFileSync(join(out, to), relink(readFileSync(file, "utf8"), source));
        console.log(`  ${to}`);
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
