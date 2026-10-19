// Diagrams follow the site's theme toggle, not only the OS (#185). Each SVG
// under docs/images/ keeps its dark colours in a `prefers-color-scheme`
// block, so it reads right on GitHub. An SVG loaded by <img> can't see the
// page's `.dark` class, though, so the build writes a light and a dark copy
// of each into docs/images/themed/ (not committed), and pages show the one
// that matches the theme.
import fs from "node:fs";
import path from "node:path";
import type MarkdownIt from "markdown-it";

const DARK = "@media (prefers-color-scheme: dark) {";

// The SVG with its dark block dropped (`light`) or unwrapped (`dark`), so
// its rules apply unconditionally after the light ones.
export function themedSvg(svg: string, theme: "light" | "dark"): string {
  const start = svg.indexOf(DARK);
  if (start < 0) return svg;
  let depth = 1;
  let end = start + DARK.length;
  for (; end < svg.length && depth > 0; end++) {
    if (svg[end] === "{") depth++;
    else if (svg[end] === "}") depth--;
  }
  const inner = svg.slice(start + DARK.length, end - 1);
  return svg.slice(0, start) + (theme === "dark" ? inner : "") + svg.slice(end);
}

// Writes images/themed/<path>.light.svg and .dark.svg for each SVG under
// images/ that has a dark block.
export function writeThemedSvgs(docsDir: string): void {
  const images = path.join(docsDir, "images");
  const out = path.join(images, "themed");
  fs.rmSync(out, { recursive: true, force: true });
  for (const rel of fs.readdirSync(images, { recursive: true }) as string[]) {
    if (!rel.endsWith(".svg") || rel.startsWith("themed")) continue;
    const svg = fs.readFileSync(path.join(images, rel), "utf8");
    if (!svg.includes(DARK)) continue;
    for (const theme of ["light", "dark"] as const) {
      const file = path.join(out, rel.replace(/\.svg$/, `.${theme}.svg`));
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, themedSvg(svg, theme));
    }
  }
}

// Renders `![alt](images/x.svg)` as two images, `images/themed/x.light.svg`
// and `.dark.svg`, which custom.css shows one at a time.
export function themedImages(md: MarkdownIt, docsDir: string): void {
  const render = md.renderer.rules.image!;
  md.renderer.rules.image = (tokens, i, opts, env, self) => {
    const token = tokens[i];
    const src = token.attrGet("src") ?? "";
    const m = src.match(/^(\.\/)?images\/(.+)\.svg$/);
    if (!m || !fs.existsSync(path.join(docsDir, "images", "themed", `${m[2]}.light.svg`))) {
      return render(tokens, i, opts, env, self);
    }
    let html = "";
    for (const theme of ["light", "dark"]) {
      token.attrSet("src", `./images/themed/${m[2]}.${theme}.svg`);
      token.attrSet("class", `only-${theme}`);
      html += render(tokens, i, opts, env, self);
    }
    return html;
  };
}
