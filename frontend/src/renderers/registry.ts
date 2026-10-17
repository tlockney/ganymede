/**
 * The `Renderer` seam: one MIME type to DOM, chosen by rank.
 *
 * A `MimeBundle` (from display_data, execute_result, or a notebook file)
 * maps MIME types to payloads. The registry picks the best type it can
 * render and delegates to that renderer. Lower rank wins.
 */

import type { JsonObject, MimeBundle } from "../protocol/messages.js";

export type { MimeBundle };

export const WIDGET_VIEW_MIME = "application/vnd.jupyter.widget-view+json";

/** Default preference order, most preferred first. */
export const DEFAULT_MIME_ORDER: readonly string[] = [
  WIDGET_VIEW_MIME,
  "text/html",
  "image/svg+xml",
  "image/png",
  "image/jpeg",
  "text/markdown",
  "text/latex",
  "application/json",
  "text/plain",
];

const UNRANKED = 1000;

/** Rank for a MIME type in the default order; unknown types sort last. */
export function defaultRank(mimeType: string): number {
  const index = DEFAULT_MIME_ORDER.indexOf(mimeType);
  return index === -1 ? UNRANKED : index;
}

export interface RenderContext {
  /**
   * Whether the output came from code this user ran in this session.
   * Untrusted HTML and SVG must not execute scripts; see basic.ts.
   */
  trusted: boolean;
}

export interface MimeRenderer {
  mimeTypes: string[];
  /** Lower is preferred. Use `defaultRank(mime)` for the standard order. */
  rank: number;
  /**
   * Render `data` into `host`, replacing any previous content.
   * `metadata` is the per-MIME-type metadata (`output.metadata[mimeType]`).
   */
  render(data: unknown, metadata: JsonObject, host: HTMLElement, context?: RenderContext): void;
}

export class RendererRegistry {
  private readonly byMime = new Map<string, MimeRenderer[]>();

  /** Register a renderer for each of its MIME types. Returns an unregister function. */
  register(renderer: MimeRenderer): () => void {
    for (const mime of renderer.mimeTypes) {
      const list = this.byMime.get(mime) ?? [];
      list.push(renderer);
      list.sort((a, b) => a.rank - b.rank);
      this.byMime.set(mime, list);
    }
    return () => {
      for (const mime of renderer.mimeTypes) {
        const list = (this.byMime.get(mime) ?? []).filter((r) => r !== renderer);
        if (list.length) this.byMime.set(mime, list);
        else this.byMime.delete(mime);
      }
    };
  }

  /** The best-ranked renderer for one MIME type. */
  rendererFor(mimeType: string): MimeRenderer | undefined {
    return this.byMime.get(mimeType)?.[0];
  }

  mimeTypes(): string[] {
    return [...this.byMime.keys()];
  }

  /**
   * Among the bundle's MIME types that have a renderer, the one with the
   * lowest rank. Ties keep bundle key order. `undefined` if nothing matches.
   */
  preferredMimeType(bundle: MimeBundle): string | undefined {
    let best: { mime: string; rank: number } | undefined;
    for (const mime of Object.keys(bundle)) {
      const renderer = this.rendererFor(mime);
      if (!renderer) continue;
      if (!best || renderer.rank < best.rank) {
        best = { mime, rank: renderer.rank };
      }
    }
    return best?.mime;
  }

  /**
   * Render the preferred type from a bundle. Returns the MIME type used, or
   * `undefined` when nothing in the bundle is renderable (host untouched).
   */
  render(
    bundle: MimeBundle,
    metadata: Record<string, unknown>,
    host: HTMLElement,
    context: RenderContext = { trusted: false },
  ): string | undefined {
    const mime = this.preferredMimeType(bundle);
    if (mime === undefined) return undefined;
    const renderer = this.rendererFor(mime)!;
    const mimeMetadata = (metadata[mime] as JsonObject | undefined) ?? {};
    renderer.render(bundle[mime], mimeMetadata, host, context);
    return mime;
  }
}
