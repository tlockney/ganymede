## 2026-11-07, a kernel_info_reply that doesn't parse (#504)

A kernel whose `kernel_info_reply` lacks `status` or `protocol_version`
answered every nudge, yet the host dropped each answer and waited until
the start timed out, with nothing in the log to say why. The nudge now
logs the parse error and the reply's content once per start, and keeps
waiting as before, so a kernel that fixes itself mid-start still works.
Found in the 0.7.0 code review (#440).
