## 2026-11-17, chrono to jiff (#855)

chrono is unmaintained (RUSTSEC-2026-0543), so every crate now uses jiff.
We took jiff over `time`
for one call site: `ganymede-server`'s notebook byline wants the local
date, and `time`'s local offset refuses to work once a program has more
than one thread on Unix, so the byline would have fallen back to UTC.
jiff reads the system time zone. The release on crates.io is 1.0.0, not
0.2; there `Zoned` is `ZonedDateTime`. chrono's `serde` feature was unused,
so jiff's is off.

Each timestamp format has a test against a fixed instant, and those
strings were recorded from chrono before the port. The same "now,
microseconds, `Z`" is built once per crate (api, protocol, terminals) as
there is no crate they all depend on; jiff's `%6f` is the fixed
six-digit, truncating fraction that chrono's `%.6f` was.

What to know:
- The exec header-date reader (`ganymede-exec`'s `timestamp`) keeps the
  wall time as written and drops the offset, so `...05.1234567+02:00`
  comes out as `03:04:05.123456Z`. jiff's own parser won't read a civil
  time with an offset or a fraction longer than nine digits, and is looser
  about separators, so the reader is small and strict, and a table test
  pins its edge inputs. Two chrono quirks are not kept: a `:60` leap second
  and a signed year come back unchanged now.
- chrono's default `to_rfc3339` (settings files' times in the lab routes)
  writes `+00:00` and a fraction of 0, 3, 6 or 9 digits, so that is
  spelled out by hand. Before 1970 jiff's sub-second part is negative, so
  it takes the remainder, not the field.
- wasm: `js` is jiff's counterpart of chrono's `wasmbind`. It adds js-sys
  only on `wasm32-unknown-unknown`, so the cfg stays the same as before
  and `wasm32-wasip1` uses its real clock.
- A `SystemTime` out of jiff's range (years +-9999) is an I/O error in the
  conflict check and is clamped in `created` and `last_modified`.
- The error text for an unparseable `last_modified` is jiff's now.
- chrono is still in the tree through rmcp and schemars, and the
  advisory fires on any copy in the tree, so the `deny.toml` ignore stays,
  now naming rmcp. It goes when rmcp moves off chrono.
