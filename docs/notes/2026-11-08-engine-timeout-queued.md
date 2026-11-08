## 2026-11-08, engine: no interrupt for a request that never started (#555)

`Kernel::execute_with` interrupted the kernel after any reply timeout.
On a shared kernel, a request still queued behind another client's cell
times out without having run, and that interrupt stopped the other
client's cell. `execute_with` now wraps the caller's handler in one
that notes the request's `busy` status, output or input request, and
interrupts only when one arrived, as the MCP scratch run already did
with its `Started` handler. A lag that skips a quiet cell's `busy` leaves
it uninterrupted; the cell keeps running, which was the behaviour for
every timed-out cell before the interrupt was added.
