## 2026-11-03, port reservations shared between processes (#327)

The cdp-node kernel that died in `parallel_starts` on macOS logged
`port 25766 still in use after 5000 ms` and then `EADDRINUSE` (and 27500
in an earlier run). The host's port reservation (#261) only covered the
process that made it, and the e2e tests run `ganymede` subprocesses
(`cli_*`) beside the in-process kernels. Two processes could each find
a port free, hand it out, and the second kernel to bind waited five
seconds and exited.

Each reservation is now also an empty file named for the port in
`<temp>/ganymede-ports-<uid>` (mode 0700, checked to be this user's own
directory, not a symlink). A file younger than five minutes keeps other
ganymede processes off the port; an older one is taken over. A process
removes the expired files the first time it uses the directory. Taking
over or removing an expired file happens under a lock on `lock` in the
directory, so two processes can't both win one; creating a fresh file
needs no lock. If the directory can't be made or written, only the
in-process map applies, as before.

The crate's own unit tests don't use the directory unless a test points
them at one: `concurrent_allocations_never_share_a_port` hands out 2,000
ports, and a few runs within five minutes filled the range for every
ganymede process of the user.

cdp-kernel's report of the port's holders reads `/proc`, so it says
`unknown` on macOS.
