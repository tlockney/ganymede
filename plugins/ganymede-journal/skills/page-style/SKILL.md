---
name: page-style
description: Format a standalone HTML page (a report, summary, artifact or share page) in the style of Ganymede's published notebooks. Use when making a page to read or share that isn't a published notebook, or when asked to match the look of a Ganymede page.
---

# Ganymede page style

Ganymede publishes a notebook as one self-contained HTML page
(`notebook_publish`, `ganymede nb publish`). Use this skill for any other
page that should look like one: a report on several journals, a summary
for a person, a page an artifact tool hosts. For a notebook itself, call
`notebook_publish` instead of writing the page by hand.

The stylesheet is [page.css](page.css), the one the exporter embeds.
Put it in a `<style>` element as it is; don't restyle what it covers.
Add rules after it only for parts it lacks, built from its variables.

## Rules

- One file. Inline the style, images (`data:` URLs, SVG inline or as a
  `data:` image) and any script. No fonts, scripts, styles or images
  from the network: the page must read the same offline and in a
  sandbox that blocks requests.
- No script unless the page needs one to work, and then a short inline
  one. Nothing that fetches.
- Light and dark from the reader's setting: use the variables, never a
  fixed colour. A page that offers a switch sets `data-theme="light"` or
  `"dark"` on `<html>`.
- Only what the reader should see: no local paths, hostnames, tokens or
  names a cell happened to print. Read the page once for them before
  you share it.
- `<html lang="en">`, `<meta charset="utf-8">`, the viewport meta, and
  a `<title>` that is the page's question or subject.

## Palette and type

Variables on `:root`, redefined for dark:

| Variable | Use |
| --- | --- |
| `--bg`, `--surface`, `--alt` | page, cards and output boxes, inline code and code blocks |
| `--fg`, `--soft` | body text, secondary text (meta, captions, table heads) |
| `--line` | rules and borders |
| `--accent`, `--accent-soft` | links, the code border, buttons (teal) |
| `--person`, `--person-soft` | anything a person wrote, as opposed to the agent (amber) |
| `--error`, `--error-soft` | errors |
| `--figure` | behind charts and images |
| `--serif`, `--sans`, `--mono` | prose; labels, meta, tables; code |

Prose is serif at 18px (17px on phones) in a column of 48rem. Labels,
meta lines, tables and buttons are sans; code is mono on `--alt`.

## Structure

```html
<div class="layout with-toc">            <!-- drop with-toc without a nav -->
  <nav class="toc" aria-label="Contents">
    <p class="toc-title">Contents</p>
    <ol><li><a href="#s1">First question</a></li></ol>
  </nav>
  <main>
    <header class="masthead">
      <h1>The question the page answers</h1>
      <p class="meta">18 Nov 2026<span class="dot">·</span>TypeScript</p>
    </header>
    <article>
      <section class="cell note"><h2 id="s1">First question</h2><p>…</p></section>
      <section class="cell run">
        <details class="code"><summary>Code · TypeScript</summary>
          <pre class="source"><code>…</code></pre></details>
        <div class="outputs">
          <figure class="image"><img src="data:image/svg+xml;base64,…" alt="…"></figure>
        </div>
      </section>
    </article>
    <footer class="colophon">Made from <span class="file">journal/…</span>.</footer>
  </main>
</div>
```

- The contents column shows on screens 72rem and wider; give each `h2`
  an `id` and list them in the nav. Leave the nav out for a short page.
- Sections are `h2`, one question each, with the finding in prose
  under the evidence.
- Code is folded under `details.code`, closed by default; readers open
  it. Highlighting classes are `hl-k` (keyword), `hl-s` (string),
  `hl-n` (number), `hl-c` (comment).
- Charts and images go in `figure.image`. Tables go in
  `<div class="table"><div class="table-scroll"><table>`, numbers in
  `td.num` (right-aligned).
- Plain output is `pre.text` or `pre.stream`; an error is `pre.error`.
- A cell a person wrote is `section.cell.by-person` (an amber bar,
  labelled "Person"); a step that ran outside the kernel is
  `section.cell.recorded`.
- Print hides folded code and the nav.

When the page shows HTML you didn't write (a library's output), put it
in a frame that can neither run scripts nor load anything, as the
exporter does:

- `<iframe class="frame" sandbox referrerpolicy="no-referrer"
  title="Output" style="height: …" srcdoc="…">`, with no
  `allow-scripts`.
- The `srcdoc` document starts with this policy, before the output, so
  the output can only narrow it: `<meta http-equiv="Content-Security-Policy"
  content="default-src 'none'; img-src data:; style-src 'unsafe-inline';
  font-src data:; form-action 'none'">`. `sandbox` alone blocks scripts,
  not loads: an `<img>` or stylesheet from the network would still fetch.
- Escape `&` and `"` in the `srcdoc` value.
- `.frame` is 12rem tall; set a height that fits the output. (The
  exporter's own page script sizes its frames; a hand-written page
  either sets the height or carries a short inline script that does.)
