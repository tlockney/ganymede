## 2026-11-08, a dropped kernel session closes its transport (#564)

`KernelSession::start_with` spawned `route` with a strong `Arc<Inner>`.
`Inner` owns the transport sink, whose socket tasks own the sender of the
channel `route` reads, so the three kept each other alive: a session
dropped without `shutdown` never closed its ZMQ sockets. `route` now
holds a `Weak<Inner>` and upgrades it only while it handles a message,
as the heartbeat monitor and process watcher already did. When the last
`KernelSession` drops, `ZmqTransport`'s `Drop` aborts its socket tasks,
the channel closes, and `route` ends.

`Inner` now also runs `mark_dead` on drop, so `watch_alive` receivers
see `false`, waiting requests fail, and `close()` runs for transports
that free resources only there, instead of relying on each sink's own
`Drop`.
