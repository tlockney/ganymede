## 2026-11-03, ganymede-ffi start gives up (#332)

`ganymede_server_start` waited on `recv()` for the server thread's first
report. Anything that stalled `run_with` before it bound the port would
have held the app's start forever with nothing to show. It now waits 30
seconds (`START_TIMEOUT`) and returns an error the app shows.

The stuck thread can't be joined, so it is left running. Before
returning, the call puts a stop message in the thread's stop channel.
The message stays buffered after the sender is dropped, so if the server
does listen later, `serve_until` reads it and stops. Dropping the sender
with nothing sent would not work: `stop.recv()` then yields `None`, and
the `Some(())` arm of the `select!` never fires.

What prompted it was a Thread Performance Checker report from the Apple
app, with the backtrace in this `recv`. That report was about priority,
not a hang: the app waited on the server thread from a user-initiated
task, and Rust threads run at Default QoS. ganymede-ui#192 starts the
server at `.medium` priority instead.
