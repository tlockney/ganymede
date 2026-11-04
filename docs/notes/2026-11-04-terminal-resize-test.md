## 2026-11-04, macOS terminal resize test reports what it saw (#368)

`names_echo_resize_and_delete` failed once more on macOS with `stty never
reported 40 132`, after #345's retries, on a PR that didn't touch
`ganymede-terminals`. It doesn't fail on Linux, and the cause can't be
reproduced from a Linux container.

portable-pty's `resize` is one `ioctl(TIOCSWINSZ)` on the master fd, and
`stty size` reads `TIOCGWINSZ` on the shell's tty, so the two should agree
at once. The test now checks the first half directly (the master reports
40×132 straight after the resize), and a failure prints every try's
output with the size the master held at that moment. The next failure
will tell whether the size was lost on the pty or `stty` printed something
the test didn't expect (an error, or the output split by a prompt redraw).
