## 2026-10-18, connection files private from the start (#49)

`write_connection_file` wrote the file with the default mode (0644 under
the usual umask) and only then chmodded it to 0600. In between, any local
user could read the HMAC key and sign messages to the kernel. It now
follows `jupyter_core.paths.secure_write`:

- Remove any existing file, then create it with `create_new` and mode
  0600, so the key is never readable by others and a symlink planted in a
  shared runtime dir is replaced instead of written through. Removing
  first keeps restarts working: a restart writes the same
  `kernel-<id>.json` again.
- A runtime dir the host creates is made 0700. An existing dir keeps its
  mode; the user may have chosen it.
- `kernel_id` goes into a file name, so it must be letters, digits, `-`
  and `_` (128 at most). `LaunchOptions.kernel_id` is public, and callers
  other than the API's UUIDs could pass `../x`.

