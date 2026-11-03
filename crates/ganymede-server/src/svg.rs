//! SVG outputs as PNG, for `ganymede mcp`: MCP image content carries
//! raster formats only, and most chart libraries (Observable Plot,
//! Vega-Lite) draw SVG. The notebook keeps the SVG; only the tool reply
//! gets the PNG.

/// SVG sources larger than this aren't rasterized.
pub const MAX_SVG_BYTES: usize = 2 * 1024 * 1024;
#[cfg(feature = "svg")]
/// The longer side of the PNG, at most; larger drawings are scaled down.
pub const MAX_SIDE: u32 = 1600;

/// `svg` as PNG bytes, scaled to fit `MAX_SIDE`. `None` when the source
/// is too large, isn't SVG, draws nothing, or the crate was built without
/// the `svg` feature.
pub fn to_png(svg: &str) -> Option<Vec<u8>> {
    if svg.len() > MAX_SVG_BYTES {
        return None;
    }
    render(svg)
}

#[cfg(feature = "svg")]
fn render(svg: &str) -> Option<Vec<u8>> {
    use resvg::{tiny_skia, usvg};
    use std::sync::{Arc, OnceLock};

    // Loading system fonts reads every font file once; keep the result.
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    let fontdb = FONTS
        .get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            db.load_system_fonts();
            Arc::new(db)
        })
        .clone();
    let options = usvg::Options {
        fontdb,
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_str(svg, &options).ok()?;
    let size = tree.size();
    let longest = size.width().max(size.height());
    if !longest.is_finite() || longest <= 0.0 {
        return None;
    }
    let scale = (MAX_SIDE as f32 / longest).min(1.0);
    let width = ((size.width() * scale).ceil() as u32).max(1);
    let height = ((size.height() * scale).ceil() as u32).max(1);
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    // Charts often leave the background transparent, which most viewers
    // show as black or a checkerboard: paint it white, as a notebook does.
    pixmap.fill(tiny_skia::Color::WHITE);
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().ok()
}

#[cfg(not(feature = "svg"))]
fn render(_svg: &str) -> Option<Vec<u8>> {
    None
}

#[cfg(all(test, feature = "svg"))]
mod tests {
    use super::*;

    fn png_size(png: &[u8]) -> (u32, u32) {
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let be = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        (be(&png[16..20]), be(&png[20..24]))
    }

    #[test]
    fn draws_an_svg_at_its_size() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="80">
            <rect x="10" y="10" width="50" height="30" fill="steelblue"/></svg>"#;
        let png = to_png(svg).expect("a PNG");
        assert_eq!(png_size(&png), (120, 80));
    }

    #[test]
    fn scales_large_drawings_down() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="6400" height="3200"/>"#;
        let png = to_png(svg).expect("a PNG");
        assert_eq!(png_size(&png), (MAX_SIDE, MAX_SIDE / 2));
    }

    #[test]
    fn huge_sizes_stay_within_the_cap() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="1e9" height="10"/>"#;
        if let Some(png) = to_png(svg) {
            let (w, h) = png_size(&png);
            assert!(w <= MAX_SIDE && h >= 1, "{w}x{h}");
        }
    }

    #[test]
    fn refuses_what_it_cannot_draw() {
        assert_eq!(to_png("not svg"), None);
        assert_eq!(to_png("<svg"), None);
        let big = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><!--{}--></svg>"#,
            "x".repeat(MAX_SVG_BYTES)
        );
        assert_eq!(to_png(&big), None);
    }
}
