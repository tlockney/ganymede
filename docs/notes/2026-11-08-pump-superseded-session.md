## 2026-11-08, pump backlog warning and replaced sessions (#579)

#568 moved the API's pump to a lossless channel, which took away the
`pump lagged` warning: a backlog could grow with no sign of it. The pump
now logs a warning when its queue reaches 10 000 messages, once until
the queue falls below half.

A restart replaces the kernel's session while the old session's pump may
still be forwarding what it had queued, so the old kernel's output and
`status` could reach clients after the new kernel had started, and set
the kernel's state from the wrong process. Each pump now forwards
through `forward_from`, which drops a message when its session is no
longer the kernel's. After a crash this changes nothing: the old pump
forwards everything before the crash handler starts the new session.
