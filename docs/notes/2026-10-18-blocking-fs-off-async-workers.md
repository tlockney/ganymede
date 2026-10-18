## 2026-10-18, file I/O off the async workers; nbconfig PATCH serialized (#70)

Several HTTP handlers did blocking `std::fs` work on tokio's async
workers: `/api/config/{section}`, `/api/kernelspecs` (which reads every
kernelspec directory and its resource files on every call), and the
JupyterLab settings, workspaces and licenses endpoints. A slow disk
stalled every connection served by that worker.

- A `routes::blocking` helper runs a closure on `spawn_blocking`; those
  handlers' bodies now run inside it.
- `PATCH /api/config/{section}` was an unlocked read-merge-write with a
  plain `fs::write`: two PATCHes at once lost one update, and a crash
  mid-write left a truncated file. PATCHes now hold a mutex, and the file
  is replaced with the same temp-file-and-rename `write_atomic` the lab
  settings use. A test sends 16 concurrent PATCHes and fails without the
  lock.

Left as is: `FsKernelspecs::list` still reads the disk on each call (now
off the async workers), as `jupyter_client` does.
