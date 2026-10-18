## 2026-10-18, docs title links to the home page (#128)

The "Ganymede" title in the docs' top bar linked to the docs index. It now
goes to the landing page at the site's root, through VitePress's
`themeConfig.logoLink`. The link needs a `target` (`_self`): VitePress's
router intercepts same-origin links without one and would try to load the
root as a docs page. The nav's Home item uses the same trick.
