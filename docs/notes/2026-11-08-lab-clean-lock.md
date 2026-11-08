## 2026-11-08, `lab clean` leaves a running install alone (#562)

`lab clean` removed every dot-named entry in the installs directories,
including the download and staging directory of an install running in
another process. Installs (`lab install` and `lab extension install`) now
hold a shared lock on `.lock` in their directory while they run, and
`lab clean` takes it exclusively and skips that directory with a
message when it can't, going on to the other one. Shared, so installs still run side by side: their entries have
unique names. Uses `std::fs::File::lock_shared`/`try_lock` (Rust 1.89),
so no new dependency.

The lock is best-effort both ways: where a filesystem has no working
locks (NFS without lockd, some FUSE or SMB mounts) installs and `clean`
go ahead unlocked, as before, with a warning. An install that finds
`clean` running says it is waiting for it.
