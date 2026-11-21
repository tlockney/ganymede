## 2026-11-21, Machine mount follow-ups (#899)

Items 1, 3 and 4 of #899, left from the connected-machine mount (#894).
Item 2 (a published page opened through the mount's `files` gets the
files policy, without `allow-downloads`) is not done here and stays open
on the issue.

- **Kernel logos.** `route` now allows `kernelspecs/{name}/{file}`, exactly
  that deep, as `Kind::Other`, with the same sandbox policy as `files`.
  The forwarded `api/kernelspecs` still lists the machine's own paths in
  `resources`, so a page shows a machine's logos only if it rebases them
  below `/machines/{id}/` (docs/embedding.md says how). The apps don't show
  kernel logos yet; that is a ganymede-ui issue. `kernelspecs`,
  `kernelspecs/a` and `kernelspecs/a/b/c` are still refused. The machine's
  own `/kernelspecs/{name}/{*file}` takes any depth; logos are flat, so the
  mount does not forward more.
- **Listing machines.** `ganymede_server_machines` returns the mounted ids,
  sorted, as a JSON array string the caller frees with
  `ganymede_string_free`, so the app need not keep its own list. NULL for a
  NULL server; `[]` for no machines, and in a build without the `gateway`
  feature. Tested in `mounts_and_unmounts_a_machine` and in
  `tests/smoke.c`.
- **Removing a machine ends its requests.** Until now only websockets
  closed. `Machine::forward` now races the request against the machine's
  `closed` signal (the one `Upstream` waits on): a machine removed or
  replaced before or while it answers is a 502, `machine {id} was
  removed`, at once, not after the 30 second response timeout. The
  `Forwarded` body keeps its own receiver, and `into_stream` ends with an
  error when the machine is closed between chunks (checked first, so an
  already removed machine ends it at once). The response is then dropped,
  which closes the connection to the machine.
  Tests, in `crates/ganymede-gateway/tests/mount.rs`, use a response
  timeout of an hour so only the removal can end them:
  `removing_a_machine_ends_a_request_it_has_not_answered`,
  `replacing_a_machine_ends_a_request_it_has_not_answered`,
  `a_request_that_arrives_at_a_removed_machine_is_not_sent`,
  `removing_a_machine_ends_a_response_that_is_streaming` and
  `a_body_of_a_removed_machine_ends_at_once`.

Finding: `Machine::close` used `watch::Sender::send`, which keeps the old
value when nothing is subscribed. With no websocket open, a removal
therefore never set `closed`, and a request that reached the machine
after it (a handle taken before the removal) would not have seen it. It
now uses `send_replace`.
`a_request_that_arrives_at_a_removed_machine_is_not_sent` found it.
