## 2026-11-07, a limit on LSP header blocks (#480)

The LSP relay capped message bodies at 64 MiB but read header lines with
an unbounded `read_line`, so a language server that never ended a header
line could make it buffer without limit. `read_message` now reads each
line through `take` and fails once a header block passes `MAX_HEADER`
(8 KiB; real headers are under 100 bytes). Blank lines between messages
don't count, since servers may pad with them. Found in the 0.7.0 code
review (#440).
