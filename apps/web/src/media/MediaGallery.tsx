// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/media-messages.md` sections 10 and 11 — the full-screen viewer over
 * every photo and video of one message (or of the composer tray): black
 * ground, the current item, `2 / 5` at the top when there are several, Save,
 * Close. The arrow keys and the side buttons move through the items; Escape
 * or a click on the ground closes it.
 *
 * The viewer holds no bytes of its own: each item says where its URL comes
 * from (already decrypted, or loaded on demand through the media hub), so a
 * video downloads only once it is the one shown.
 */
import { createEffect, createSignal, on, onCleanup, onMount, Show, type JSX } from "solid-js";
import { Portal } from "solid-js/web";

import { t } from "../l10n";
import { Icon } from "../ui/icons";
import { paintVideoBehind } from "./video-overlay";

export interface GalleryItem {
  readonly kind: "photo" | "video";
  /** The URL already at hand, or `null`. Read reactively. */
  readonly url: () => string | null;
  /** Fetch the URL when there is none yet; `null` when it cannot be had. */
  readonly load?: () => Promise<string | null>;
  readonly saveName: string;
  /** A video element started playing this item (the hub's one player). */
  readonly onVideo?: (el: HTMLVideoElement) => void;
  /** The item stopped being shown. */
  readonly onHide?: () => void;
}

/** The index after a key press, or `null` when the key does not move. Wraps
 *  neither way: the first and last items are ends. */
export function galleryStep(index: number, total: number, key: string, rtl: boolean): number | null {
  const forward = rtl ? "ArrowLeft" : "ArrowRight";
  const back = rtl ? "ArrowRight" : "ArrowLeft";
  if (key === forward && index + 1 < total) return index + 1;
  if (key === back && index > 0) return index - 1;
  if (key === "Home" && index > 0) return 0;
  if (key === "End" && index + 1 < total) return total - 1;
  return null;
}

function saveObjectUrl(url: string, filename: string): void {
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.style.display = "none";
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
}

export function MediaGallery(props: {
  readonly items: readonly GalleryItem[];
  readonly start: number;
  readonly onClose: () => void;
}): JSX.Element {
  const [index, setIndex] = createSignal(Math.max(0, Math.min(props.start, props.items.length - 1)));
  const [failed, setFailed] = createSignal(false);
  const [loading, setLoading] = createSignal(false);
  let closeButton: HTMLButtonElement | undefined;
  let player: HTMLVideoElement | undefined;
  const current = (): GalleryItem | undefined => props.items[index()];
  const total = (): number => props.items.length;
  const rtl = (): boolean => globalThis.document?.documentElement.dir === "rtl";

  // What hides the item on screen, taken when it is shown: the viewer's own
  // cleanup runs while its parent is torn down, where the parent's getters
  // (`props.items`) can throw, so it must not read them then.
  let hideShown: (() => void) | undefined;
  function hide(): void {
    const h = hideShown;
    hideShown = undefined;
    h?.();
  }

  // Load the shown item when it has no URL yet.
  createEffect(
    on(index, (i) => {
      setFailed(false);
      const item = props.items[i];
      hideShown = item?.onHide;
      if (item === undefined || item.url() !== null || item.load === undefined) return;
      setLoading(true);
      void item.load().then(
        (u) => {
          if (index() !== i) return;
          setLoading(false);
          if (u === null) setFailed(true);
        },
        () => {
          if (index() !== i) return;
          setLoading(false);
          setFailed(true);
        },
      );
    }),
  );

  function go(next: number): void {
    if (next === index() || next < 0 || next >= total()) return;
    player?.pause();
    player = undefined;
    hide();
    setIndex(next);
  }

  function close(): void {
    player?.pause();
    player = undefined;
    hide();
    props.onClose();
  }

  const onKey = (e: KeyboardEvent): void => {
    if (e.key === "Escape") {
      e.preventDefault();
      close();
      return;
    }
    // A key meant for the video's own controls stays with them.
    if (e.target instanceof HTMLVideoElement) return;
    const next = galleryStep(index(), total(), e.key, rtl());
    if (next !== null) {
      e.preventDefault();
      go(next);
    }
  };
  onMount(() => {
    globalThis.document?.addEventListener("keydown", onKey);
    closeButton?.focus();
  });
  onCleanup(() => {
    globalThis.document?.removeEventListener("keydown", onKey);
    player?.pause();
    hide();
  });

  const shownUrl = (): string | null => current()?.url() ?? null;

  return (
    <Portal>
      <div
        class="lk-viewer"
        role="dialog"
        aria-modal="true"
        aria-label={current()?.kind === "video" ? t("chat.media.video") : t("chat.media.photo")}
        onClick={(e) => {
          if (e.target === e.currentTarget) close();
        }}
      >
        <Show when={total() > 1}>
          <div class="lk-viewer-position" aria-live="polite">
            {t("chat.viewer.position", { n: index() + 1, total: total() })}
          </div>
        </Show>
        <div class="lk-viewer-bar">
          <Show when={shownUrl()}>
            {(u) => (
              <button type="button" onClick={() => saveObjectUrl(u(), current()?.saveName ?? "media")}>
                {t("common.save")}
              </button>
            )}
          </Show>
          <button ref={closeButton} type="button" aria-label={t("common.close")} onClick={close}>
            {t("common.close")}
          </button>
        </div>
        <Show when={total() > 1 && index() > 0}>
          <button
            type="button"
            class="lk-viewer-step back"
            aria-label={t("chat.viewer.previous")}
            title={t("chat.viewer.previous")}
            onClick={() => go(index() - 1)}
          >
            <Icon name="chevron-back" size={28} />
          </button>
        </Show>
        <Show when={total() > 1 && index() + 1 < total()}>
          <button
            type="button"
            class="lk-viewer-step forward"
            aria-label={t("chat.viewer.next")}
            title={t("chat.viewer.next")}
            onClick={() => go(index() + 1)}
          >
            <Icon name="chevron-forward" size={28} />
          </button>
        </Show>
        <Show
          when={shownUrl()}
          keyed
          fallback={
            <div class="lk-viewer-wait">
              <Show
                when={failed()}
                fallback={<Show when={loading()}>{<span class="lakko-spinner" />}</Show>}
              >
                <span>
                  {current()?.kind === "video" ? t("chat.video.load_failed") : t("chat.photo.load_failed")}
                </span>
              </Show>
            </div>
          }
        >
          {(u) => (
            <Show
              when={current()?.kind === "video"}
              fallback={<img src={u} alt={t("chat.media.photo")} draggable={false} />}
            >
              <video
                ref={(el) => {
                  player = el;
                  current()?.onVideo?.(el);
                  paintVideoBehind(el);
                }}
                src={u}
                controls
                autoplay
                playsinline
              />
            </Show>
          )}
        </Show>
      </div>
    </Portal>
  );
}
