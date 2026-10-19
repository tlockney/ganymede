## 2026-10-19, diagrams follow the site theme (#185)

- An SVG loaded through `<img>` is its own document: its
  `prefers-color-scheme` query sees the OS, never the page's `.dark` class
  or `data-theme`. So a reader who picked dark on a light OS got light
  diagrams. Keeping the media query in the source (GitHub has no toggle,
  so following the OS is right there) and having the docs build write a
  light and a dark copy keeps one file to edit per diagram. The markdown
  image rule swaps in both copies, and CSS hides the one that doesn't match.
- The landing page is plain HTML outside VitePress, so it references the
  generated copies directly; the pages workflow checks they exist before
  deploying and after.
- `jupyter.svg` put its point, "needs a Python environment", in grey
  caption text. It is now a pill on the box, and both stack diagrams have
  a legend for what colour and dashes mean. Grey microcopy is darker
  (#4b535d) in light mode and lighter (#b6bcc4) in dark.
