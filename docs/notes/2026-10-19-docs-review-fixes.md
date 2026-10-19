## 2026-10-19, docs review: stale statements (#177)

A review of all the docs ([report](https://github.com/tlockney/ganymede/issues/177))
found the Design pages still written as the original sketch's plans, and
status lines on many pages that had gone stale as work landed. This entry
fixes the statements that were wrong; restructuring and diagrams follow
in their own pull requests.

- Fixed: the FFI layer and the Mac/iPad app called planned
  (architecture, ipados), frontend extensions called unsupported
  (architecture), the remote provisioner called next and terminals
  called planned (python-free, protocol), nbformat 3 called unread
  (protocol), the widget manager described as anywidget-only
  (python-free), the roadmap's Next listing the UI, the landing page's
  "Own UI: Planned", and README's crate table missing `ganymede-gateway`
  and `ganymede-ffi`.
- Lesson: a status line in a design page goes stale the day the work
  lands, and nobody greps design pages when merging a feature. Status
  belongs in Roadmap and Known gaps; the restructuring PR moves it there.
- Not touched here: Security's cross-origin text, which #170 corrects
  along with the CORS behaviour, and `ui.md`, which #176 edits.
