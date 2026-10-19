## 2026-10-19, lab extension config written atomically (#172)

`ganymede lab extension disable/enable` wrote `labconfig/page_config.json`, and `lab extension install` wrote its install records, with `std::fs::write`, which truncates and then writes. Both files are read with tolerant readers, so a crash between the two silently undid every disable or made an install unknown to `uninstall` and `list`. Both now go through `nbformat::atomic_write` (temp file, fsync, rename), as notebooks and config sections do. The test holds a hard link to the old file and checks it still has the old bytes after the write, which only holds when the file is replaced rather than rewritten in place.

Lesson: any file read with a fall-back-to-defaults reader needs an atomic writer, since a truncated file reads as "nothing set" without an error.
