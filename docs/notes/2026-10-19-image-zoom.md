## 2026-10-19, image zoom in the docs (#197)

VitePress has no image zoom of its own. The theme now attaches
[medium-zoom](https://github.com/francoischalifour/medium-zoom) to every
image in a page that isn't inside a link (`docs/.vitepress/theme/zoom.ts`),
again on each route change.

- medium-zoom treats a source ending in `.svg` as having no natural size
  and scales it to fill the viewport; a PNG grows only to its natural size.
  Vite keeps our diagrams as `.svg` files (all are over the 4 KB inline
  limit); an SVG inlined as a `data:` URI would stop at its `width`.
- The zoomed copy is the same `<img>`, so a diagram keeps its own
  `prefers-color-scheme` colors. The backdrop is `--vp-c-bg`.
- The overlay needs `z-index` above the nav bar and sidebar (60).
- The ganymede-ui pages under /docs/ganymede-ui/ use this theme, so they
  get it too. cdp-kernel.org has the same file (cdp-kernel#367).
