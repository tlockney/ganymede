## 2026-10-19, one name for getting started (#186, #190)

- The header's "Get started" and the landing page's main button went to
  Getting started, which assumes Ganymede is installed. Both now go to
  Install, and the sidebar group, the landing section and the overview's
  section are all "Get started", the label the nav already used. The
  Getting started page keeps its title: it is the second step.
- The reading-level pill sits closer to the title, so it reads as the
  title's eyebrow rather than a separate line. Scrollbars are thin and
  use the theme's divider colour.
- The outline still ends long headings with an ellipsis. Wrapping them
  would break the folded-section trick in `OutlineItem.vue`, which
  relies on every row being 32px tall; the full heading shows on hover.
