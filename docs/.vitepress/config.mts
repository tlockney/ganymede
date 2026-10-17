// Builds docs/*.md into <site>/docs/ (see .github/workflows/pages.yml). The
// Markdown stays the source of truth and keeps working on GitHub; this file
// only adds navigation and rewrites links that leave docs/. The theme is
// cdp-kernel.org's, so the two related sites read alike.
import path from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vitepress";
import { groups } from "./levels.ts";

const repo = "https://github.com/tlockney/ganymede";
// Where the site is served: the custom domain set in the repository's Pages
// settings. Its path becomes the docs' base, so it must match the domain
// Pages serves from, or every asset 404s and the docs render unstyled.
const site = (process.env.SITE_URL ?? "https://ganymede-server.com").replace(/\/$/, "");
const base = `${new URL(site).pathname.replace(/\/$/, "")}/docs/`;
const docsDir = path.resolve(import.meta.dirname, "..");
const repoDir = path.resolve(docsDir, "..");

// Pages link to each other as `install.md`, which VitePress resolves. Links
// to anything else (crates/, validation/) point at files VitePress doesn't
// publish, so send them to GitHub.
function externalHref(href: string, page: string): string | undefined {
  if (/^[a-z][a-z0-9+.-]*:|^#|^\//i.test(href)) return;
  const [target, hash = ""] = href.split(/(?=#)/);
  const abs = path.resolve(path.dirname(page), target);
  if (abs.startsWith(docsDir + path.sep) && abs.endsWith(".md")) return;
  const rel = path.relative(repoDir, abs).split(path.sep).join("/");
  return `${repo}/${target.endsWith("/") ? "tree" : "blob"}/main/${rel}${hash}`;
}

export default defineConfig({
  title: "Ganymede",
  description: "A Jupyter server in Rust with no Python in the product.",
  base,
  cleanUrls: true,
  rewrites: { "README.md": "index.md" },
  sitemap: { hostname: `${site}/docs/` },
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
  // theme/OutlineItem.vue.
  vite: {
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
    nav: [
      { text: "Home", link: `${site}/`, target: "_self", noIcon: true },
      { text: "Get started", link: "/getting-started" },
      { text: "Guides", link: "/frontends" },
      { text: "Design", link: "/architecture" },
      { text: "Roadmap", link: "/roadmap" },
    ],
    // Groups start folded; VitePress opens the one holding the current page.
    sidebar: groups().map(({ text, items }) => ({ text, items, collapsed: true })),
    outline: [2, 3],
    search: { provider: "local" },
    editLink: { pattern: `${repo}/edit/main/docs/:path`, text: "Edit this page on GitHub" },
    socialLinks: [{ icon: "github", link: repo }],
    footer: { message: "MIT license." },
  },
});
