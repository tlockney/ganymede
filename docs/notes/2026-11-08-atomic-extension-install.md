## 2026-11-08, atomic extension install (#578)

`lab extension install` moved a wheel's extensions into place one at a
time, after it had already removed the old version's and rewritten other
packages' records. A move that failed partway left a mix of old, new and
missing extensions, with records that matched none of it.

`place` now does the moves as one step: everything it replaces or
removes first goes to a backup directory beside the extensions, then the
new extensions move in. If a move fails, the new ones go back to staging
and the old ones come back; if one can't come back, the backup is kept
and the error names it. Records change only after `place` succeeds. A
test puts a file where a scope directory should go to fail the second
move.
