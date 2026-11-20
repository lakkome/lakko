// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/media-messages.md` section 11 — the photos and videos of one message
 * laid out together in its bubble: one tile per item, in the order sent, at
 * the place the shared rule gives it (`media-mosaic` in `lakko-ui-rules`,
 * mapped by `mosaic-layout.ts`). One item is the same rule's single layout, so
 * a lone photo and an album are drawn by one component.
 *
 * Each tile keeps section 10's states on its own: a photo loads by itself when
 * it comes near the screen (the thumb, blurred, then the full picture in its
 * place); a video shows its poster, a play glyph and its size and downloads
 * only when opened. Tapping any tile opens the viewer on it, with every photo
 * and video of the message to move through.
 *
 * Every byte goes through the media hub, whose AEAD open fails before an
 * element sees anything.
 */
import { createMemo, createSignal, For, onMount, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { openFailureOf, type AttachmentFailure } from "../chat/attachment-failure";
import { formatAttachmentSize, type AttachmentRefBytes } from "../chat/attachments";
import { wasmSync } from "../wasm";
import { AttachmentFailureNote } from "./AttachmentFailureNote";
import { MediaGallery, type GalleryItem } from "./MediaGallery";
import type { MediaHub } from "./media-hub";
import { mediaKey } from "./media-hub";
import { mosaicBoxStyle, mosaicLayout, mosaicTileStyle, type MosaicLayout } from "./mosaic-layout";
import { whenVisible } from "./near-viewport";
import { inlineThumbDataUrl, saveNameFor } from "./visual-media";

/** The most photos and videos a mosaic shows (section 11; the rule's own
 *  constant, read once). */
export function mosaicMaxItems(): number {
  try {
    return wasmSync().mediaMosaicMaxItems();
  } catch {
    return 10;
  }
}

/** The layout of `refs` through the shared rule. */
export function layoutOf(refs: readonly AttachmentRefBytes[]): MosaicLayout {
  const w = wasmSync();
  return mosaicLayout(
    refs,
    (bubble, gap, widths, heights) => {
      const m = w.mediaMosaic(bubble, gap, widths, heights);
      try {
        return { width: m.width, height: m.height, tiles: m.tiles };
      } finally {
        m.free();
      }
    },
    w.mediaMosaicGap(),
  );
}

export interface MediaMosaicProps {
  /** The message's photos and videos, in the order sent. */
  readonly refs: readonly AttachmentRefBytes[];
  readonly hub: MediaHub;
  /** The key each item's bytes are filed under in the hub. */
  readonly keyFor?: (ref: AttachmentRefBytes) => string;
  /** The message is still being sent: a spinner on each tile. */
  readonly pending?: boolean;
  /** The meta pill (time, ticks), drawn at the bottom end of the mosaic when
   *  the message has nothing else (section 11). */
  readonly pill?: (() => JSX.Element) | undefined;
}

type Status = "idle" | "loading" | "ready" | "error";

export function MediaMosaic(props: MediaMosaicProps): JSX.Element {
  const shown = createMemo(() => props.refs.slice(0, mosaicMaxItems()));
  const layout = createMemo<MosaicLayout | null>(() => {
    try {
      return layoutOf(shown());
    } catch {
      return null;
    }
  });
  const keyOf = (ref: AttachmentRefBytes): string => props.keyFor?.(ref) ?? mediaKey(ref.blobId);
  const kindOf = (ref: AttachmentRefBytes): "photo" | "video" =>
    props.hub.decision(ref).bubble === "video" ? "video" : "photo";
  const [viewer, setViewer] = createSignal<number | null>(null);

  const galleryItems = (): GalleryItem[] =>
    shown().map((ref) => {
      const key = keyOf(ref);
      const kind = kindOf(ref);
      return {
        kind,
        url: () => props.hub.urlFor(key),
        load: async () => {
          if (ref.blobId.length === 0) return null;
          try {
            return await props.hub.load(ref);
          } catch {
            return null;
          }
        },
        saveName: saveNameFor(ref, kind),
        ...(kind === "video"
          ? {
              onVideo: (el: HTMLVideoElement) => props.hub.claim(key, () => el.pause()),
              onHide: () => props.hub.release(key),
            }
          : {}),
      };
    });

  return (
    <Show when={layout()}>
      {(l) => (
        <div class="lk-mosaic" style={mosaicBoxStyle(l())}>
          <For each={shown()}>
            {(ref, i) => (
              <Show when={l().tiles[i()]}>
                {(tile) => (
                  <MosaicTile
                    attachment={ref}
                    hub={props.hub}
                    mediaKey={keyOf(ref)}
                    kind={kindOf(ref)}
                    pending={props.pending === true}
                    style={mosaicTileStyle(tile(), l())}
                    onOpen={() => setViewer(i())}
                  />
                )}
              </Show>
            )}
          </For>
          <Show when={props.pill}>{(pill) => <div class="lk-mosaic-pill">{pill()()}</div>}</Show>
          <Show when={viewer() !== null}>
            <MediaGallery items={galleryItems()} start={viewer() ?? 0} onClose={() => setViewer(null)} />
          </Show>
        </div>
      )}
    </Show>
  );
}

function MosaicTile(props: {
  readonly attachment: AttachmentRefBytes;
  readonly hub: MediaHub;
  readonly mediaKey: string;
  readonly kind: "photo" | "video";
  readonly pending: boolean;
  readonly style: Record<string, string>;
  readonly onOpen: () => void;
}): JSX.Element {
  const [thumbUrl, setThumbUrl] = createSignal<string | null>(null);
  const [status, setStatus] = createSignal<Status>("idle");
  const [failure, setFailure] = createSignal<AttachmentFailure>("failed");
  let root: HTMLButtonElement | undefined;
  const hasRemote = (): boolean => props.attachment.blobId.length > 0;
  const fullUrl = (): string | null => (props.kind === "photo" ? props.hub.urlFor(props.mediaKey) : null);

  // ADR-0047 section 6 — the micro thumbnail inside the message (on the first
  // photo or video only) stands in until the sent thumb arrives.
  const inlineUrl = createMemo(() => inlineThumbDataUrl(props.attachment.inlineThumb));
  const placeholderUrl = (): string | null => thumbUrl() ?? inlineUrl();

  const thumbKey = (): string | null => {
    const th = props.attachment.thumb;
    return th === undefined || th.blobId.length === 0 ? null : `thumb:${mediaKey(th.blobId)}`;
  };

  function adoptThumb(): void {
    const k = thumbKey();
    const u = k === null ? null : props.hub.urlFor(k);
    if (u !== null) setThumbUrl(u);
  }

  async function loadThumb(): Promise<void> {
    const th = props.attachment.thumb;
    if (th === undefined || thumbUrl() !== null) return;
    try {
      const u = await props.hub.loadThumb(th);
      if (u !== null) setThumbUrl(u);
    } catch {
      // A failed thumb is not a failed item: the full picture may still load.
    }
  }

  async function loadFull(): Promise<void> {
    if (props.kind !== "photo" || fullUrl() !== null || !hasRemote()) return;
    setStatus("loading");
    try {
      await props.hub.load(props.attachment);
      setStatus("ready");
    } catch (err) {
      setFailure(openFailureOf(err));
      setStatus("error");
    }
  }

  onMount(adoptThumb);
  whenVisible(
    () => root,
    () => {
      void loadThumb();
      void loadFull();
    },
  );

  const label = (): string => {
    if (status() === "error" && failure() === "gone") return t("chat.attachment.gone.title");
    if (status() === "error" && failure() === "limit") return t("chat.attachment.limit.open");
    if (props.kind === "video") {
      return status() === "error"
        ? t("chat.video.load_failed_retry")
        : t("chat.video.play", { size: formatAttachmentSize(props.attachment.size) });
    }
    if (status() === "error") return t("chat.photo.load_failed_retry");
    if (status() === "loading") return t("chat.photo.loading");
    return t("chat.media.photo");
  };

  return (
    <button
      ref={root}
      type="button"
      class="lk-mosaic-tile"
      style={props.style}
      aria-label={label()}
      onClick={(e) => {
        e.stopPropagation();
        if (status() === "error") {
          if (failure() !== "gone") void loadFull();
          return;
        }
        props.onOpen();
      }}
    >
      <Show
        when={fullUrl()}
        fallback={
          <Show when={placeholderUrl()}>
            {(u) => <img class={props.kind === "photo" ? "thumb" : undefined} src={u()} alt="" draggable={false} />}
          </Show>
        }
      >
        {(u) => <img src={u()} alt="" draggable={false} />}
      </Show>
      <Show when={props.kind === "video"}>
        <span class="lk-visual-center" aria-hidden="true">
          <span class="lk-visual-play lk-mosaic-play">
            <Show when={props.pending} fallback={<span>▶</span>}>
              <span class="lakko-spinner" />
            </Show>
          </span>
        </span>
        <Show when={props.attachment.size > 0}>
          <span class="lk-visual-size" aria-hidden="true">
            {formatAttachmentSize(props.attachment.size)}
          </span>
        </Show>
      </Show>
      <Show when={props.kind === "photo" && (status() === "loading" || props.pending)}>
        <span class="lk-visual-corner" aria-hidden="true">
          <span class="lakko-spinner" />
        </span>
      </Show>
      <Show when={status() === "error"}>
        <span class="lk-visual-center">
          <span class="lk-visual-note">
            <AttachmentFailureNote failure={failure()} inline>
              {props.kind === "video" ? t("chat.video.load_failed") : t("chat.photo.load_failed")}
            </AttachmentFailureNote>
          </span>
        </span>
      </Show>
    </button>
  );
}
