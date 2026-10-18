## 2026-10-18, a shutdown_reply that only reached iopub (#122)

`interrupt_and_shutdown_requests_on_shell` failed now and then on macOS
with `KernelDied`. The 500 ms linger after exit didn't help, because the
reply was never coming. With ten copies of the test running in parallel
it fails on Linux too, about 1 run in 100, always on python3. strace on
the kernel showed that the `shutdown_reply` on shell was never written to
the socket in the failing runs. Only the copy ipykernel broadcasts on
iopub went out.

The cause is in ipykernel 7. A reply on shell goes from the main thread
to the shell-channel thread, which queues it on a ZMQStream. A shutdown
then stops that thread, sometimes before the stream has flushed, and the
queued reply is dropped. jupyter_client loses it the same way: a script
that sends `shutdown_request` on shell right after other requests missed
3 replies in 250 runs.

What changed: when the kernel dies, a request still waiting for its reply
is answered with the `shutdown_reply` the kernel broadcast on iopub for
it, if there was one. The reply on the request's own socket is still
preferred: the iopub copy is used only once the kernel is dead, after the
linger. The test's unwraps now name the kernel that failed.

What we learned: when a reply goes missing, first check whether it ever
left the kernel (strace on `sendto`) before looking for a race in the
host.
