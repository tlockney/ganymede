## 2026-11-08, Markdown export: bad base64 falls back to text (#525)

The Markdown export wrote `![png](<name>_files/output_N_M.png)` before the
server decoded the image, so an image whose base64 didn't decode left the
file out of the zip and the link dangling. Decoding moved into
`ganymede-nbformat`: `ResourceData::Base64(String)` became
`ResourceData::Bytes(Vec<u8>)`, and an output picks the first type in
priority order that it can show. A PNG or JPEG that doesn't decode is
skipped, so the output falls back to its next entry, usually `text/plain`,
as an output with no image does. A cell attachment that doesn't decode is
left out, and its `attachment:` reference stays as written. The server's
zip writer now only copies bytes.
