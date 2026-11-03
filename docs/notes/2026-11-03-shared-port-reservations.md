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
removes the expired files the first time it uses the directory. If the
directory can't be made or written, only the in-process map applies, as
before.

cdp-kernel's report of the port's holders reads `/proc`, so it says
`unknown` on macOS.
