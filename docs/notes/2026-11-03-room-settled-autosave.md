## 2026-11-03, room saves while its file keeps changing (#347)

- A room that reloaded a settled file change pushed its autosave back
  each time, so a file rewritten every 0.5 to 1 s (or a loaded CI runner
  that stalled past the 500 ms debounce) kept the room's own edits from
  ever reaching disk. The never-settles test on main failed this way.
- The room now saves right after any reload from disk when it has edits
  of its own, settled or forced by the five-second cap. Saving just
  after taking in the file makes a 412 from a newer write unlikely.
- The busy-file test helper now re-reads the file just before each
  rename, so its read-modify-write no longer overwrites a room save and
  hides it. A second test writes every 0.7 s, so each change settles.
