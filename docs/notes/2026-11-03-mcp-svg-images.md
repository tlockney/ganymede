## 2026-11-03, SVG outputs as PNG for the agent (#312)

MCP image content carries raster formats only, so with `--images` an
agent got no picture of an Observable Plot or Vega-Lite chart, which
render `image/svg+xml`. `ganymede mcp` now rasterizes an output's SVG
to PNG for the tool reply when the output has no raster image of its
own; the notebook keeps the SVG.

- **resvg**, pure Rust (Apache-2.0 OR MIT, linebender), behind a new
  `svg` feature of ganymede-server, on by default. ganymede-ffi builds
  ganymede-server without default features, so the Apple apps don't
  carry it. resvg's default features are off: `memmap-fonts` would map
  font files with `unsafe`, and `raster-images` (PNG/JPEG/GIF inside an
  SVG) pulls in decoders the charts don't need. `text` and
  `system-fonts` stay on: axis labels are text, and without fonts they
  vanish. The font database loads once per process; the first SVG pays
  for scanning the system's fonts.
- **Caps.** SVG sources over 2 MB are skipped before parsing. The PNG is
  scaled to at most 1,600 px on its longer side, which bounds the
  pixmap: a `width="1e9"` SVG can't allocate gigabytes.
- **No local files.** usvg's default `<image href>` resolver reads any
  local path that exists, so an SVG output could make the server read
  `/dev/zero` until it ran out of memory, hang on a FIFO, or draw a
  local file into the agent's image. The renderer takes images only from
  `data:` URLs. Turning off resvg's `raster-images` feature doesn't stop
  the read; it only skips drawing raster formats.
- **Off the runtime.** The first SVG scans the system's fonts and each
  one rasterizes up to 1,600 px square, so the tool calls build their
  image blocks in `spawn_blocking`; under `serve --mcp` the runtime also
  serves the server's websockets.
- **White background.** Charts usually leave the background
  transparent, which many viewers show as black; a notebook shows white.
- An SVG that doesn't parse, or has no size, adds no image; the text
  part of the reply still names the output's `image/svg+xml`, so the
  call doesn't fail.
