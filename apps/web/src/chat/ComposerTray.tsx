// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/media-messages.md` section 11 — the composer tray: a strip above
 * the message field with what the message will carry. A photo or video is a
 * 64 px square tile (a video with its length on a pill), a file a 64 px chip
 * with its name and size. Each item shows its upload on a thin ring and, if
 * the upload failed, a retry glyph; the (x) in the top end corner removes it.
 * Clicking a photo or video opens the viewer over the tray's photos and
 * videos. Hidden while empty. The limit line sits under it for 4 s.
 */
import { createSignal, Index, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { Icon } from "../ui/icons";
import { MediaGallery, type GalleryItem } from "../media/MediaGallery";
import { formatAttachmentSize } from "./attachments";
import { isVisual, trayDurationLabel } from "./compose-tray";
import type { TrayItem, TrayStore } from "./tray-store";

/** The ring's circumference, for a radius of 14 in a 32 x 32 box. */
const RING_C = 2 * Math.PI * 14;

function itemName(item: TrayItem): string {
  if (item.name.length > 0) return item.name;
  return item.kind === "video" ? t("chat.media.video") : item.kind === "photo" ? t("chat.media.photo") : t("chat.media.file");
}

function Progress(props: { readonly item: TrayItem }): JSX.Element {
  return (
    <Show when={props.item.status === "preparing" || props.item.status === "uploading"}>
      <span class="lk-tray-ring" aria-hidden="true">
        <svg viewBox="0 0 32 32" width="32" height="32">
          <circle cx="16" cy="16" r="14" class="track" />
          <circle
            cx="16"
            cy="16"
            r="14"
            class="bar"
            stroke-dasharray={`${RING_C}`}
            stroke-dashoffset={`${RING_C * (1 - Math.max(0.04, props.item.progress))}`}
          />
        </svg>
      </span>
    </Show>
  );
}

export function ComposerTray(props: {
  readonly tray: TrayStore;
  /** The composer is busy: nothing can be removed or retried. */
  readonly disabled?: boolean;
}): JSX.Element {
  const [viewer, setViewer] = createSignal<number | null>(null);
  const visual = (): TrayItem[] => props.tray.items().filter((i) => isVisual(i.kind));

  const galleryItems = (): GalleryItem[] =>
    visual().map((item) => ({
      kind: item.kind === "video" ? "video" : "photo",
      url: () => props.tray.items().find((i) => i.id === item.id)?.fullUrl ?? null,
      saveName: item.file.name.length > 0 ? item.file.name : item.kind === "video" ? "video.mp4" : "photo.jpg",
    }));

  let strip: HTMLDivElement | undefined;
  const onWheel = (e: WheelEvent): void => {
    if (strip === undefined || strip.scrollWidth <= strip.clientWidth) return;
    // A vertical wheel scrolls the strip sideways (section 11).
    if (Math.abs(e.deltaY) > Math.abs(e.deltaX)) {
      strip.scrollLeft += e.deltaY;
      e.preventDefault();
    }
  };

  return (
    <Show when={props.tray.items().length > 0 || props.tray.notice() !== null}>
      <div class="lk-tray">
        <Show when={props.tray.items().length > 0}>
          <div class="lk-tray-strip" ref={strip} onWheel={onWheel} role="list">
            <Index each={props.tray.items()}>
              {(slot) => {
                const item = slot;
                // A refused upload and one with no answer both show the retry
                // glyph (a tap retries); only the refused one stops Send.
                const failed = (): boolean => item().status === "failed" || item().status === "waiting";
                const name = (): string => itemName(item());
                const open = (): void => {
                  if (failed()) {
                    props.tray.retry(item().id);
                    return;
                  }
                  if (!isVisual(item().kind)) return;
                  const at = visual().findIndex((v) => v.id === item().id);
                  if (at >= 0) setViewer(at);
                };
                return (
                  <div class="lk-tray-item" classList={{ file: item().kind === "file", failed: failed() }} role="listitem">
                    <button
                      type="button"
                      class="lk-tray-face"
                      aria-label={failed() ? t("chat.tray.retry", { name: name() }) : name()}
                      title={failed() ? t("chat.tray.retry", { name: name() }) : name()}
                      disabled={props.disabled === true || (!failed() && item().kind === "file")}
                      onClick={open}
                    >
                      <Show
                        when={item().kind !== "file"}
                        fallback={
                          <>
                            <span class="lk-tray-doc" aria-hidden="true">
                              <Icon name="doc" size={22} />
                            </span>
                            <span class="lk-tray-file-text">
                              <span class="lk-tray-file-name">{name()}</span>
                              <span class="lk-tray-file-size">{formatAttachmentSize(item().size)}</span>
                            </span>
                          </>
                        }
                      >
                        <Show when={item().thumbUrl} fallback={<span class="lk-tray-blank" />}>
                          {(u) => <img src={u()} alt="" draggable={false} />}
                        </Show>
                        <Show when={item().kind === "video" ? trayDurationLabel(item().durationMs) : null}>
                          {(d) => <span class="lk-tray-duration">{d()}</span>}
                        </Show>
                      </Show>
                      <Progress item={item()} />
                      <Show when={failed()}>
                        <span class="lk-tray-retry" aria-hidden="true">
                          <Icon name="arrow-clockwise" size={20} />
                        </span>
                      </Show>
                    </button>
                    <button
                      type="button"
                      class="lk-tray-remove"
                      aria-label={t("chat.tray.remove", { name: name() })}
                      title={t("chat.tray.remove", { name: name() })}
                      disabled={props.disabled === true}
                      onClick={() => props.tray.remove(item().id)}
                    >
                      <Icon name="xmark" size={12} />
                    </button>
                  </div>
                );
              }}
            </Index>
          </div>
        </Show>
        <Show when={props.tray.notice()}>
          {(n) => (
            <p class="lk-tray-notice" role="status">
              {n() === "media" ? t("chat.tray.limit_media") : t("chat.tray.limit_files")}
            </p>
          )}
        </Show>
        <Show when={viewer() !== null && visual().length > 0}>
          <MediaGallery items={galleryItems()} start={viewer() ?? 0} onClose={() => setViewer(null)} />
        </Show>
      </div>
    </Show>
  );
}
