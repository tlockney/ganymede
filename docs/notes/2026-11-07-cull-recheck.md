## 2026-11-07, the culler re-checks each kernel (#506)

`cull_idle` chose its candidates once, then shut them down one at a time,
each shutdown waiting out its grace period. A client that connected to a
later candidate meanwhile, or a cell that started on it, still lost its
kernel. Each candidate is now checked again (no connections, idle, past
the limit) under its lifecycle lock, just before its shutdown, through a
new `shutdown_if`. Found in the 0.7.0 code review (#440).
