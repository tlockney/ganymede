## 2026-11-07, output order after a lag (#507)

`execute_streaming` hands iopub messages to its handler as they arrive
and, after the reply, the ones it never saw. When the handler fell behind
the session's broadcast, the messages after the gap were still handed
over live, so the skipped ones came last, out of order. After a lag it
now stops handing over live messages; the reply's record supplies the
rest in the kernel's order. The handler sees output later, not wrong.
Found in the 0.7.0 code review (#440).
