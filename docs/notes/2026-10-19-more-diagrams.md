## 2026-10-19, more diagrams (#194, #177)

Five diagrams where prose carried structure: the command map
(Getting started), where the install puts files (Install), three ways to
reach a server (Security), saving a notebook (Architecture), and the
gateway (CLI and configuration).

- The command map sorts commands by what they start: a kernel and no
  server, a server, or nothing. That is the line first-day readers miss
  between `run`/`execute` and `serve`/`lab`.
- The save diagram is drawn from `FsContents::save`: the `If-Match` and
  `last_modified` checks come before anything is written, signing comes
  after the atomic write, and the first checkpoint is made only when none
  exists. The checkpoint step is easy to get wrong, because `jupyter_server`
  makes later checkpoints only on the client's POST.
- The security diagram compares the three setups the page already
  recommends or warns against, so "Avoid" and "Do" are the page's own
  advice, not new rules.
- #194's per-profile embedding diagram is left for the embedding page
  (#189), which is where its seams and features belong.
