## 2026-11-26, privacy page marker for the name sent to connected machines (#965)

ganymede-ui#842 lets the Mac app send your account name to a connected
machine's notebook rooms, per machine and on by default. docs/privacy.md
says the name "never goes to a remote server", which stays true for the
released app. Per CLAUDE.md, the sentence keeps its words until
ganymede-ui releases, with an `Unreleased in ganymede-ui` note beside it
saying what the next release adds; ganymede-ui's release script lists the
note and opens an issue here to update it. (An HTML-comment marker is
only accepted in site/index.html; check-unreleased wants a note block in
Markdown.)
