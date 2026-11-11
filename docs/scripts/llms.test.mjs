// llms.txt and the pages' Markdown copies (docs/.vitepress/llms.ts).
import { test } from "node:test";
import assert from "node:assert/strict";
import { llmsTxt, markdownPath, rewriteLinks, sourceOf, summaryOf } from "../.vitepress/llms.ts";

test("a summary is the first sentence of the first paragraph of prose", () => {
  const md = [
    "# Install",
    "",
    "> [!NOTE]",
    "> **Unreleased.** Not this.",
    "",
    "```sh",
    "ganymede serve",
    "",
    "not this either.",
    "```",
    "",
    "<!-- nor this -->",
    "",
    "Ganymede is **one** [binary](install.md), `ganymede`. It runs anywhere.",
  ].join("\n");
  assert.equal(summaryOf(md), "Ganymede is one binary, `ganymede`.");
});

test("a summary skips lists and tables, and keeps snake_case", () => {
  assert.equal(summaryOf("# A\n\n- item\n\n| a |\n\nServes jupyter_server's API: all of it."), "Serves jupyter_server's API.");
  assert.equal(summaryOf("# A\n\n- only a list"), "");
});

test("sources and copies follow the sidebar links", () => {
  assert.equal(sourceOf("/"), "README.md");
  assert.equal(markdownPath("/"), "index.md");
  assert.equal(sourceOf("/ganymede-ui/apps"), "ganymede-ui/apps.md");
  assert.equal(markdownPath("/cell-api/jupyter"), "cell-api/jupyter.md");
});

test("llms.txt lists the built pages by group, background and contributors as optional", () => {
  const groups = [
    { text: "Get started", level: "intro", items: [{ text: "Overview", link: "/" }, { text: "Missing", link: "/ganymede-ui/gone" }] },
    { text: "Background", level: "background", items: [{ text: "Why", link: "/why" }] },
    { text: "Empty", level: "guide", items: [{ text: "Gone", link: "/gone" }] },
  ];
  const pages = { "README.md": "# Ganymede\n\nA server.", "why.md": "# Why\n\nBecause." };
  const txt = llmsTxt(groups, (s) => pages[s], "https://example.com/docs/", "A Jupyter server.");
  assert.equal(
    txt,
    [
      "# Ganymede",
      "",
      "> A Jupyter server.",
      "",
      "Each link is a page's Markdown source. The HTML page is the same URL without `.md`.",
      "",
      "## Get started",
      "",
      "- [Overview](https://example.com/docs/index.md): A server.",
      "",
      "## Optional: Background",
      "",
      "- [Why](https://example.com/docs/why.md): Because.",
      "",
    ].join("\n"),
  );
});

test("the copies' links and images are rewritten as the HTML's are", () => {
  const md = "See [install.sh](../site/install.sh), [Install](install.md#a) and ![x](images/a.svg).\n<img src=\"images/b.svg\">";
  const out = rewriteLinks(md, (href) => (href.endsWith(".md") || href.startsWith("install.md") ? undefined : `https://gh/${href}`));
  assert.equal(
    out,
    "See [install.sh](https://gh/../site/install.sh), [Install](install.md#a) and ![x](https://gh/images/a.svg).\n<img src=\"https://gh/images/b.svg\">",
  );
});
