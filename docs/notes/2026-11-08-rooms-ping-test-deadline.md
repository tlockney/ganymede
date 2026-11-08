## 2026-11-08, Rooms ping test waits on a deadline (#520)

`rooms_ping_clients_and_drop_silent_ones` waited for the silent client's
room to retire for 100 iterations of "read the live socket for up to
20 ms". The live socket gets a ping every 50 ms, and each one ends an
iteration early, so the real bound was well under 2 s, against a server
that needs about 400 ms (idle timeout, one ping interval, cleanup). A
slow macOS runner ran past it. Both waits in the test are now wall-clock
deadlines (10 s for the retire, 5 s for the room to open), and the
failure message says how long it waited. Counting loop iterations as a
time bound is unreliable whenever the loop body can return early.
