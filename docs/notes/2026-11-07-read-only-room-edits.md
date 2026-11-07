## 2026-11-07, a room turning read-only keeps its edits (#437)

A room could turn read-only with unsaved edits: after a rename whose new
path another server had locked, or when a room that was writable without
the lock (`HeldHere`, `Unavailable`) later found the lock taken. From
then on `save_room` returned `Skipped`, the edits were lost at the next
client edit (`restore_baseline`), and the cleanup kept the room open
forever because it still looked dirty.

- `save_before_relock` saves a dirty, writable room before each lock check
  that can make it read-only: on `Moved` and on the tick's retry.
- A dirty read-only room retires once its clients leave, with a warning;
  keeping it open could never save it.
- `take_lock` warns when it makes a dirty room read-only (a save that
  failed just before), instead of logging the switch at info.

The unit test holds the lock from a second `Collab`, which `flock` treats
as another process, so the room gets `Elsewhere` after its save.
