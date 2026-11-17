## 2026-11-17, websocket terminal test waits for the resize (#861)

`terminals_rest_and_websocket` failed on macOS CI: it sent `set_size` and
then `stty size` back to back and got `24 80`. The server already applies
`set_size` (a synchronous pty ioctl in `TerminalClient::resize`) before it
queues any later stdin, so the order is right. What differs is macOS: a
`stty size` run right after a resize can still see the old size for a
moment, as in #345 and #368 for the terminals crate's
`names_echo_resize_and_delete`. The REST/websocket test now checks the cwd
on its own, then asks `stty size` up to 20 times until it reports the new
size, like that test, and panics with every try's output if it never does.

Two limits. #368 saw the crate test fail once even with retries, so a
rare failure here is not by itself a regression. And this test can't
read the pty's size the way the crate test does, so it can't tell a
failed resize from a slow `stty`; the crate test is the place to look.
