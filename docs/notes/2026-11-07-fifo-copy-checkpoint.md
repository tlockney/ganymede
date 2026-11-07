## 2026-11-07, copy and checkpoint refuse a FIFO (#489)

`get` serves only regular files and directories, but `copy` and
`create_checkpoint` checked only for a directory and then `fs::read` the
source. On a FIFO that read waits for a writer that never comes, holding
a blocking thread and the request. Both now answer 404, as `get` does,
for anything that isn't a regular file. The first checkpoint a notebook
save makes goes through the same `write_checkpoint`, so it is covered too.
