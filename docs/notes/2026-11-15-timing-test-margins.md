## 2026-11-15, timing tests hold on a stalled runner (#832)

`e2e (macos-latest)` failed twice on `main` in tests that assumed the
runner schedules them promptly. `rooms_ping_clients_and_drop_silent_ones`
runs a server with a 300 ms idle timeout and reads as the live client in
100 ms slices, so a runner that stalled the test task for 300 ms made the
server drop a client that was still alive. `missed_heartbeats_mark_an_attached_kernel_dead`
(#826) slept three heartbeat periods (60 ms) and then turned the fake
kernel's beats off, but the monitor declares a kernel dead only after it
has seen one answered beat. When the runner stalled the monitor through
that sleep, no beat had been answered, and the kernel was rightly never
declared dead.

The room test now uses a 1 s idle timeout, and its first live loop runs
2.5 s, so it still proves that reading keeps a client past the timeout.
The heartbeat test's fake kernel counts the beats it answers, and the
test waits for the first one before it turns them off. A few other
waits changed the same way: the reconnect test polls for the closed
socket to leave the connection count, the kernel-host tests that gave a
kernel 300 ms to take up a request, or 1 to 2 s to be marked dead after
an exit, now allow 1 s and 10 s.

The rule: a test waits on a condition with a wall-clock deadline of
several seconds, not on a fixed sleep, and a server timeout that a live
client must beat gets a margin of about 1 s. A short timeout that only
bounds a check that something does not happen can stay short, since a
slow runner makes it pass more easily.
