## 2026-11-05, e2e: compare printed output, not its chunking (#381)

`server_matches_jupyter_server_on_ipykernel` failed on #380 because the
jupyter_server reference run got `print("hi " + input())` as two `stream`
messages, the text and then the newline, while Ganymede's run got one.
ipykernel's `OutStream` flushes on a timer, so where it splits output
depends on timing under either server.

The parity tests now join consecutive `stream` messages to the same
stream before comparing (`merge_streams`), in both runs. They still
check which stream got what text and in what order relative to other
messages.
