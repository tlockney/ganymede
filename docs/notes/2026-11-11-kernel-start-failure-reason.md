## 2026-11-11, say why a kernel didn't start (#713)

A kernel whose boot fails (it dies, or never answers `kernel_info` within
`startup_timeout`, three launches each) is reported dead and removed.
`wait_started` then found it gone and answered `kernel <id> not found`,
which is what `POST /api/notebooks/run` passed on: an app link run showed
"the kernel cdp-browser failed to start: kernel … not found", with no
hint of the cause. The boot's error is now kept on the kernel and
`wait_started` answers `kernel <id> did not start: <why>` (still 404).
