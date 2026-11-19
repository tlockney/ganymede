## 2026-11-19, /files streams from disk and answers Range (#875)

`GET /files/{path}` read the whole file into memory before it sent a
byte. The apps' data view stops at 8 MB, but the server, in process on
iOS, still read all of it. Now the file is streamed from disk in chunks
of at most 64 KiB, and a single `bytes` `Range` is answered.

- `Contents` has a new method, `open_file`, with a default that returns
  `Ok(None)`. `FsContents` opens the file on the blocking pool under the
  same root, path and hidden rules as `get`, and takes the length from
  the open handle. A directory is a 400, as before. Other
  implementations keep the default, and the route reads them with `get`
  and slices the bytes, so both paths answer `Range` the same way.
  `Stamping` forwards the call.
- `Content-Type` keeps `FilesHandler`'s rule. The name decides first. Only
  a name that gives no type needs to know whether the file is text, which
  the contents manager used to learn by decoding the whole file. The route
  now checks UTF-8 over the file in 64 KiB chunks and carries a character
  cut by a chunk's end into the next one, so memory stays flat and the
  answer is the same. A published page's generator tag is looked for in
  the first 512 bytes of an HTML file only.
- Range follows RFC 9110. One range of unit `bytes` (`a-b`, `a-`, `-n`) gets
  `206` with `Content-Range` and `Content-Length`. An end past the file
  is clamped. A start past the end, `-0`, or any range of an empty file
  gets `416` with `Content-Range: bytes */<length>`. Another unit,
  several ranges, a malformed spec, `a > b` and a number that doesn't fit
  a `u64` are ignored, and the whole file is sent. A request with
  `If-Range` is ignored too, since `/files` sends no validator. Every
  answer carries `Accept-Ranges: bytes`.
- A file that is cut short while it streams fails the stream, so the
  connection drops rather than end a body shorter than its
  `Content-Length`.
- jupyter_server's default, file-backed contents manager serves `/files`
  through tornado's `StaticFileHandler`, which answers single byte ranges
  with 206 and 416. Only a non-file contents manager uses `FilesHandler`,
  which sends the whole file. So this brings `/files` in line with
  jupyter_server's default for ranges. It is noted as unreleased in
  `docs/api.md`.
- The UTF-8 check runs before the first byte, for `HEAD` and `Range`
  too, so a Range request on a large file whose name gives no type
  (`.jsonl`, `.ndjson`, `.arrow`) reads the whole file first. Memory
  stays flat. Names with a known type (`.csv`, `.json`, `.tsv`,
  `.parquet`) skip it. Accepted to keep `FilesHandler`'s Content-Type rule.
- CORS allows the `Range` and `If-Range` request headers and exposes
  `Content-Range` and `Accept-Ranges`, so the apps' data view and other
  clients on an allowed origin can make and read range requests.
