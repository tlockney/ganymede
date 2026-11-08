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

From the review: decoding takes padded or unpadded base64, as
`base64.decodebytes` and JupyterLab do (the same lenient config as
`ganymede-contents`). An attachment uses the first image entry that
decodes, not the first image entry. `Export::skipped` lists the entries
left out, and the server logs each as before; the export itself now runs
on a blocking thread, since decoding a notebook's images can take a while.
