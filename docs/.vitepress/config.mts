// Builds docs/*.md into <site>/docs/ (see .github/workflows/pages.yml). The
// Markdown stays the source of truth and keeps working on GitHub; this file
// only adds navigation and rewrites links that leave docs/. The theme is
// cdp-kernel.org's, so the two related sites read alike.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vitepress";
import { groups } from "./levels.ts";
import { notesPlugin, withEntries } from "./notes.ts";

const repo = "https://github.com/tlockney/ganymede";
const uiRepo = "https://github.com/tlockney/ganymede-ui";
// Where the site is served: the custom domain set in the repository's Pages
// settings. Its path becomes the docs' base, so it must match the domain
// Pages serves from, or every asset 404s and the docs render unstyled.
const site = (process.env.SITE_URL ?? "https://ganymede-server.com").replace(/\/$/, "");
const base = `${new URL(site).pathname.replace(/\/$/, "")}/docs/`;
const docsDir = path.resolve(import.meta.dirname, "..");
const repoDir = path.resolve(docsDir, "..");

// ganymede-ui's docs/*.md, copied here by `npm run fetch-ui` (not
// committed) and published under /docs/ganymede-ui/. Without the copy the
// site builds without them.
const uiDir = path.join(docsDir, "ganymede-ui");
const uiPages = fs.existsSync(uiDir)
  ? fs.readdirSync(uiDir).filter((f) => f.endsWith(".md")).sort((a, b) => (a === "architecture.md" ? -1 : b === "architecture.md" ? 1 : a.localeCompare(b)))
  : [];
if (uiPages.length === 0) console.warn("docs/ganymede-ui/ is empty; run `npm run fetch-ui` to include ganymede-ui's docs.");

function titleOf(file: string): string {
  const heading = fs.readFileSync(file, "utf8").match(/^# (.+)$/m);
  return heading ? heading[1].trim() : path.basename(file, ".md");
}

// Pages link to each other as `install.md`, which VitePress resolves. Links
// to anything else (crates/, validation/) point at files VitePress doesn't
// publish, so send them to GitHub. ganymede-ui's pages sit in its docs/,
// so their links resolve against that repository.
function externalHref(href: string, page: string): string | undefined {
  if (/^[a-z][a-z0-9+.-]*:|^#|^\//i.test(href)) return;
  const [target, hash = ""] = href.split(/(?=#)/);
  const abs = path.resolve(path.dirname(page), target);
  const kind = target.endsWith("/") ? "tree" : "blob";
  if (page.startsWith(uiDir + path.sep)) {
    if (path.dirname(abs) === uiDir && abs.endsWith(".md") && fs.existsSync(abs)) return;
    const rel = path.posix.join("docs", path.relative(uiDir, abs).split(path.sep).join("/"));
    return `${uiRepo}/${kind}/main/${rel}${hash}`;
  }
  if (abs.startsWith(docsDir + path.sep) && abs.endsWith(".md")) return;
  const rel = path.relative(repoDir, abs).split(path.sep).join("/");
  return `${repo}/${kind}/main/${rel}${hash}`;
}

export default defineConfig({
  title: "Ganymede",
  description: "A Jupyter server in Rust with no Python in the product.",
  base,
  cleanUrls: true,
  rewrites: { "README.md": "index.md" },
  // Log entries are not pages of their own; notes.ts puts them on notes.md.
  srcExclude: ["notes/**", "scripts/**"],
  // A link into ganymede-ui's pages fails only the page it names, which
  // that repository can rename: without the copy, or after a rename, warn
  // instead of failing this site's build.
  ignoreDeadLinks: [
    (link: string) => {
      const m = link.match(/(?:^|\/)ganymede-ui\/([^/#?]+?)(?:\.md)?(?:[#?].*)?$/);
      if (!m || uiPages.includes(`${m[1]}.md`)) return false;
      console.warn(`ganymede-ui has no docs/${m[1]}.md; the link to it is dead.`);
      return true;
    },
  ],
  sitemap: { hostname: `${site}/docs/` },
  // The landing page's icon, from site/, served at the site's root.
  head: [["link", { rel: "icon", href: `${new URL(site).pathname.replace(/\/$/, "")}/favicon.svg`, type: "image/svg+xml" }]],
  markdown: {
    config(md) {
      const render = md.renderer.rules.link_open ?? ((tokens, i, opts, _env, self) => self.renderToken(tokens, i, opts));
      md.renderer.rules.link_open = (tokens, i, opts, env, self) => {
        const token = tokens[i];
        const href = token.attrGet("href");
        const external = href && externalHref(href, env.path);
        if (external) token.attrSet("href", external);
        return render(tokens, i, opts, env, self);
      };
    },
  },
  // Swap in a theme component that folds the "On this page" outline; see
  // theme/OutlineItem.vue. notesPlugin builds the development log page.
  vite: {
    plugins: [notesPlugin(docsDir)],
    resolve: {
      alias: [
        {
          find: /^.*\/VPDocOutlineItem\.vue$/,
          replacement: fileURLToPath(new URL("./theme/OutlineItem.vue", import.meta.url)),
        },
      ],
    },
  },
  themeConfig: {
    // The title and Home go to the landing page at the site's root, outside
    // the docs' base. target keeps VitePress's router from handling the
    // click as a docs route.
    logoLink: { link: `${site}/`, target: "_self" },
    nav: [
      { text: "Home", link: `${site}/`, target: "_self", noIcon: true },
      { text: "Get started", link: "/getting-started" },
      { text: "Guides", link: "/frontends" },
      { text: "Design", link: "/architecture" },
      { text: "Roadmap", link: "/roadmap" },
    ],
    // Groups start open, so every page shows without a click, except the
    // contributors' group, which most readers don't need.
    sidebar: [
      ...groups().map(({ text, level, items }) => ({ text, items, collapsed: level === "contributor" })),
      ...(uiPages.length
        ? [{ text: "ganymede-ui", collapsed: false, items: uiPages.map((f) => ({ text: titleOf(path.join(uiDir, f)), link: `/ganymede-ui/${f.replace(/\.md$/, "")}` })) }]
        : []),
    ],
    outline: [2, 3],
    search: {
      provider: "local",
      options: {
        _render: (src, env, md) => md.render(env.relativePath === "notes.md" ? withEntries(src, docsDir) : src, env),
      },
    },
    editLink: {
      // Serialized into the client bundle, so it can't use this module's
      // constants.
      pattern: ({ filePath }) =>
        filePath.startsWith("ganymede-ui/")
          ? `https://github.com/tlockney/ganymede-ui/edit/main/docs/${filePath.slice("ganymede-ui/".length)}`
          : `https://github.com/tlockney/ganymede/edit/main/docs/${filePath}`,
      text: "Edit this page on GitHub",
    },
    socialLinks: [{ icon: "github", link: repo }],
    footer: { message: "MIT license." },
  },
});
