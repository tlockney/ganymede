## 2026-11-04, kernel memory and CPU (#361)

`/api/metrics/v1` and `/api/metrics/v1/kernel_usage/get_usage/{id}`
follow jupyter-resource-usage, read from its source (`api.py`,
`server_extension.py`) and its lab extension's `widget.tsx`, which reads
only `content`: a reply with `content.reason` is shown as that reason,
anything else as `usage_reply` fields. ipykernel's reply carries
`hostname`, `pid`, `kernel_cpu`, `kernel_memory` (PSS where the OS has
it), `cpu_count`, `host_cpu_percent` (left out when 0) and
`host_virtual_memory`.

jupyter-resource-usage asks every kernel and waits ten seconds; most
non-Python kernels never answer, so here the first `usage_request` gets
two seconds, and a session that misses it is measured from the host
from then on. The kernel must have finished starting first, or a slow
start would be taken for a kernel that never answers.

Measuring uses sysinfo (only its `system` feature) on Linux, macOS and
Windows: a process's descendants come from parent ids over a full
process scan, and Linux threads, which it lists as processes, are
skipped. CPU is relative to the previous scan, so `System` lives in the
server state and the first reading is 0, as psutil's is.
