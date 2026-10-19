## 2026-10-19, architecture diagrams (#177)

Step 2 of the docs review. Architecture gets two SVG diagrams, and the
stack diagram is brought up to date.

- `crates.svg`: the eleven crates and what each uses, from the
  `[dependencies]` of each `Cargo.toml`, with edges implied by another
  edge left out so it stays readable (the cli's direct use of contents,
  for one). The table under it lost its "Depends on" column.
- `running-a-cell.svg`: a sequence diagram with two clients on one
  kernel, which shows the routing rule the numbered list described: iopub
  to every client, the shell reply only to the one that asked, and done
  when both the reply and the matching idle have arrived.
- `ganymede.svg`: Notebook 7 removed (its pages aren't served; clients.md
  says so), ganymede-ui and other clients added, gateway and ffi crates
  added, and the "in-process, iPad, planned" kernel replaced by remote
  kernels through the gateway. Taller, so the landing page's `<img>`
  height changed with it.
- The SVGs are generated in the style of the existing ones (same classes
  and dark-mode block) and were checked as rendered in Chromium, light and
  dark, before committing.
