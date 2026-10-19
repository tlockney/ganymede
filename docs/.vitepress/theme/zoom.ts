import mediumZoom, { type Zoom } from "medium-zoom";

let zoom: Zoom | undefined;

// Click an image in a page to open it in a modal that fills the viewport
// (#197). medium-zoom scales an SVG up to the viewport and a raster image up
// to its natural size. Images inside a link keep the link instead.
export function attachZoom(): void {
  zoom ??= mediumZoom({ background: "var(--vp-c-bg)", margin: 24 });
  zoom.detach();
  const images = document.querySelectorAll<HTMLImageElement>(".vp-doc img");
  zoom.attach(...[...images].filter((img) => !img.closest("a")));
}
