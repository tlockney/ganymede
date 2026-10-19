## 2026-10-19, landing hero and screenshot (#179, #180, #181, #182)

- The screenshot was re-shot from a real `ganymede lab` with cdp-kernel
  0.7.0 on Node 24, in a 820×740 viewport at 2× with the file browser
  closed. At the README's ~850px width that is about CSS size, so the
  code reads; the old 1280px shot shrank text to about 7px. Two copies,
  `jupyterlab.png` and `jupyterlab-dark.png`, from JupyterLab's light
  and dark themes. The README picks one with `<picture>`; the landing
  page shows one by theme with the same rules as its diagrams.
- To re-shoot: execute the notebook with `ganymede execute --inplace`,
  serve with a `--config-dir` whose `lab/user-settings` sets the theme
  and turns off the news prompt (`@jupyterlab/apputils-extension`
  `notification`: `fetchNews: "false"`), and press Ctrl+B before the
  screenshot to close the left area.
- The hero's install block has to fit `curl ... | sh` (55 characters)
  without scrolling, which needs a text column of about 510px; the hero
  stacks below 1180px wide for that. The page's content width went from
  1040px to 1200px so the screenshot beside the text stays legible.
