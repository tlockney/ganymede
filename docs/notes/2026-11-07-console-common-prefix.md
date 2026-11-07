## 2026-11-07, a common-prefix completion keeps the rest of the word (#484)

#479 made a console completion replace up to the kernel's `cursor_end`.
But `ganymede console` uses rustyline's list completion, which, with
several candidates, inserts their longest common prefix through the same
`update` call. So `x.ap|pendleft`, with `append` and `appendleft`
offered, became `x.append`: text after the cursor was deleted though no
candidate was picked.

Now only a single candidate replaces up to `cursor_end` (`replace_end`);
a common prefix replaces what is before the cursor, as rustyline does by
default. A `cursor_start` past the cursor drops the completion instead
of moving the start back, which made `pri|nt` with the range 4..5 and
match `t` into `prit`.
