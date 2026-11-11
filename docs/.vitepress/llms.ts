// llms.txt (https://llmstxt.org) for the docs: an index an agent can read
// to find the page it needs, and each page's Markdown beside its HTML, so
// /docs/install.md is the source of /docs/install (#689). The index follows
// the sidebar (levels.ts) and links the Markdown copies.
import fs from "node:fs";
import path from "node:path";
import type { Group } from "./levels.ts";

/** The source file under docs/ for a sidebar link: "/" is README.md. */
export function sourceOf(link: string): string {
  return link === "/" ? "README.md" : `${link.replace(/^\//, "")}.md`;
}

/** Where a page's Markdown copy is published, under the docs' base. */
export function markdownPath(link: string): string {
  return link === "/" ? "index.md" : `${link.replace(/^\//, "")}.md`;
}

/**
 * A page's first paragraph of prose, as plain text, up to its first
 * sentence: what the index says about the page. Skips notes, code, HTML,
 * images and lists.
 */
export function summaryOf(markdown: string): string {
  const body = markdown
    .replace(/^---\n[\s\S]*?\n---\n/, "")
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/^(`{3,}|~{3,})[^\n]*\n[\s\S]*?^\1[^\n]*$/gm, "");
  for (const block of body.split(/\n\s*\n/)) {
    const text = block.trim();
    if (!text || /^(#|>|<|!\[|[-*+] |\d+\. |\||:::|@\[)/.test(text)) continue;
    const plain = text
      .replace(/!\[[^\]]*\]\([^)]*\)/g, "")
      .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
      .replace(/\*{1,2}([^*]+)\*{1,2}/g, "$1")
      .replace(/\s+/g, " ")
      .trim();
    const sentence = plain.match(/^.+?[.!?:](?=\s|$)/);
    return (sentence ? sentence[0] : plain).replace(/:$/, ".");
  }
  return "";
}

/**
 * llms.txt for `groups`. `read(source)` returns a page's Markdown, or
 * undefined when the page wasn't built (a ganymede-ui page without the
 * copy); `url` is the docs' absolute base, ending in "/".
 */
export function llmsTxt(groups: Group[], read: (source: string) => string | undefined, url: string, description: string): string {
  const lines = [
    "# Ganymede",
    "",
    `> ${description}`,
    "",
    "Each link is a page's Markdown source. The HTML page is the same URL without `.md`.",
  ];
  for (const group of groups) {
    const items = group.items.flatMap((page) => {
      const md = read(sourceOf(page.link));
      if (md === undefined) return [];
      const summary = summaryOf(md);
      return [`- [${page.text}](${url}${markdownPath(page.link)})${summary ? `: ${summary}` : ""}`];
    });
    if (items.length === 0) continue;
    // llms.txt's "Optional" section holds what a reader can skip.
    const heading = group.level === "background" || group.level === "contributor" ? `Optional: ${group.text}` : group.text;
    lines.push("", `## ${heading}`, "", ...items);
  }
  return `${lines.join("\n")}\n`;
}

/**
 * `markdown` with each link and image target passed through `rewrite`,
 * which returns a new target or undefined to keep it: the HTML pages
 * rewrite links that leave docs/, and the copies must too.
 */
export function rewriteLinks(markdown: string, rewrite: (href: string) => string | undefined): string {
  return markdown
    .replace(/(\]\()(<[^>]+>|[^)\s]+)/g, (m, open, href) => open + (rewrite(href.replace(/^<|>$/g, "")) ?? href))
    .replace(/(\bsrc=")([^"]+)/g, (m, open, href) => open + (rewrite(href) ?? href));
}

/**
 * Writes llms.txt and each page's Markdown into the built site at
 * `outDir`. `rewrite(href, file)` is the link rewrite the HTML gets.
 */
export function writeLlms(
  outDir: string,
  docsDir: string,
  groups: Group[],
  url: string,
  description: string,
  rewrite: (href: string, file: string) => string | undefined = () => undefined,
): void {
  const read = (source: string) => {
    const file = path.join(docsDir, source);
    return fs.existsSync(file) ? fs.readFileSync(file, "utf8") : undefined;
  };
  for (const page of groups.flatMap((g) => g.items)) {
    const md = read(sourceOf(page.link));
    if (md === undefined) continue;
    const out = path.join(outDir, markdownPath(page.link));
    const file = path.join(docsDir, sourceOf(page.link));
    fs.mkdirSync(path.dirname(out), { recursive: true });
    fs.writeFileSync(out, rewriteLinks(md, (href) => rewrite(href, file)));
  }
  fs.writeFileSync(path.join(outDir, "llms.txt"), llmsTxt(groups, read, url, description));
}
