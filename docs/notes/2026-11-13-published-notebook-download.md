## 2026-11-13, download the notebook from a published page (#751)

Thomas wanted a published page (`nb publish`, `notebook_publish`, the
journal's Publish step) to give the notebook back, accepting what the
page scrubs. The page now carries a copy as `<script
type="application/json">` and a Download notebook button under the
title.

Choices:

- The copy is built from the notebook, not from the page, and keeps to
  the page's rules: the same redactions over every string, kernelspec
  and language as the only notebook metadata, a raw cell's format and
  an agent cell's `by`/`mode` as the only cell metadata. Recorded
  parameters are dropped rather than kept as names or placeholders: the
  run's values can be secrets, and the notebook runs with its defaults
  without them.
- Base64 bitmap entries skip redaction, so a replacement can't corrupt
  an image. The page decodes and re-encodes them, so neither form adds
  text the page doesn't show.
- `<`, `>` and `&` go in as `\u` escapes, so nothing in a string can
  close the script element; the button re-serializes with nbformat's
  one-space indent, which restores them.
- On by default, off with hidden code (the copy holds the code the
  publisher hid), with `--no-download` / `download: false` to drop it.
  The page with images roughly doubles: the Oklahoma journal went from
  602 KB to 1.1 MB. Pointing the page's images at the copy would save
  that, but would make the page depend on its script to show images.
- Earlier artifact experiments found the claude.ai `downloads`
  capability refuses `.ipynb`. With that capability the script saves a
  zip holding the notebook (stored, written in about twenty lines, CRC
  included); elsewhere a blob link saves the `.ipynb`. The journal skill
  says to declare `downloads` when hosting the page as an artifact.

Checked in Chromium with the Oklahoma journal: the plain download and
the zip (through a stand-in `downloads` capability) both hold the same
notebook, 54 cells and 29 outputs, with no `/tmp` or home path left.
