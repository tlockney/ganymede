## 2026-10-18, landing page, README, and docs navigation (#67)

The review's presentation list, minus the GitHub repository settings
(description, topics, https homepage, social preview), which only an
owner can change from the repository's settings page.

- The landing page's first install tab is the install script; `cargo`
  needs a Rust toolchain, which the audience for a Python-free server
  often doesn't have. A line under it says which platforms are tested.
- cdp-kernel is in the top navigation and in a "who is this for" line in
  the hero, not only the footer.
- The page has a canonical link, `og:`/`twitter:` tags, and an SVG
  favicon that the docs use too. `og:image` is `images/jupyterlab.png`, a
  JupyterLab screenshot; the pages workflow copies `docs/images/` to
  `/images/`, so one file serves the README, the docs, and the card.
- "No Python" is qualified where it's claimed: no Python runtime or
  dependency, with the three places Python does appear (the PyPI wheel
  `lab install` unpacks, the CI reference, the nbformat byte format) in
  the README's Status section and the landing page's feature card.
- The landing page's client table said JupyterLab "Needs cookie login";
  cookie login shipped in #30 (#66's last open item). The footer links
  the security page (#65's last open item) and the changelog.
- The README gained badges, a Features list, and a Quick start, as
  cdp-kernel's has. It and the roadmap said JupyterLab extensions are
  unsupported while `ganymede lab extension install` exists; they now say
  server extensions.
- Docs: a Changelog page that includes `CHANGELOG.md` (as cdp-kernel's
  does), and a Known gaps page collecting what was spread over
  frontends.md, python-free.md, install.md, and the roadmap. Sidebar
  groups start open, except Contributing, so "Replacing Python" no longer
  hides behind a fold.

Not done: a shared theme package or CI check against cdp-kernel's copy of
the CSS (#67 item 9). The two sites' styles are still kept in step by
hand.
