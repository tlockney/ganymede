## 2026-11-08, removals respect a running install (#570)

`lab remove`, `lab uninstall` and `lab extension remove` take the
exclusive lock that `lab clean` takes (#562) and fail, removing nothing,
while an install holds the shared lock. They don't wait: an install can
run for minutes, and a removal that silently waits looks hung. `clean`
still skips instead of failing, since its leftovers can wait. The lock
logic is one `exclusive_lock` for all four; where locks don't work they
go ahead unlocked, as before.
