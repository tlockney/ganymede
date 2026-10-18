## 2026-10-18, If-Match on contents saves (#75)

The last open item of #75: the `last_modified` check lets an external
write slip through when it lands within 500 ms of the client's copy, and
it compares timestamps, not content. Thomas chose to add an exact,
opt-in precondition.

- A content `GET /api/contents/{path}` and every file or notebook `PUT`
  answer with `ETag: "<sha256>"`, the same hash `?hash=1` reports. The
  body keeps jupyter_server's null `hash` unless `?hash=1` was asked, so
  the JSON is unchanged. `?content=0` gets no ETag, since that would
  mean reading the file only to hash it.
- `PUT` with `If-Match` is 412 unless the file exists and its sha256 is
  one of the listed tags. `*` only needs the file to exist. Weak tags
  (`W/"..."`) never match, per RFC 9110's strong comparison, so a header
  of only weak tags fails.
- The check runs inside the per-path save lock with the write, so two
  saves naming the same tag cannot both pass. Chunked uploads check it on
  chunk 1 only; later chunks append to the upload in progress.
- The hash comes back from `Contents::save` in the model's `hash`, so
  embedders (the Swift shell) get it without a second read; the server
  moves it to the header.
- CORS now allows `If-Match` and exposes `ETag`, or a browser client on
  another origin could neither send nor read them.

Tornado already sets an `Etag` (sha1 of the response body) on
jupyter_server's GET responses, and no Jupyter client sends `If-Match`
on saves, so the header is additive: existing clients see no change.
