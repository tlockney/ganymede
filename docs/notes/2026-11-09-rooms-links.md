## 2026-11-09, rooms links point at rooms.md (#592)

#411 moved the description of document rooms into `docs/rooms.md` and
left `protocol.md`'s "Document model and collaboration" as a short design
note that links there. The roadmap's "Document rooms" entry and
`api.md`'s "Every route" still linked that note for how rooms and their
endpoints work, so a reader took an extra hop. Both now link
`rooms.md`. Found by the weekly docs freshness check.
