// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX spec `docs/ux/media-messages.md` section 10 — photo and video bubbles in
 * the timeline, and the full-screen viewer they open.
 *
 * The bubble is chosen before this file is reached (`renderDecision`: no
 * `kind` and an allowlisted image or `video/mp4` type). The box is fixed from
 * `width` / `height` (`bubbleBox`) so nothing jumps when bytes land.
 *
 *   * A photo loads on its own once it scrolls into view: the thumb first
 *     (blurred), then the full image, which replaces it in place.
 *   * A video shows its poster, a play button and the file size, and
 *     downloads only when tapped; the download then opens the player.
 *
 * Every byte goes through the media hub, whose AEAD open fails before an
 * element sees anything, and whose registry keeps decrypted URLs for the life
 * of the chat view (so scrolling back does not download again) and revokes
 * them when the view closes.
 */

import { t } from "../l10n";
import { AttachmentFailureNote } from "./AttachmentFailureNote";
import { openFailureOf, type AttachmentFailure } from "../chat/attachment-failure";
import { createEffect, createMemo, createSignal, on, onCleanup, onMount, Show, type JSX } from "solid-js";
import { Portal } from "solid-js/web";

import { formatAttachmentSize } from "../chat/attachments";
import type { MediaBubbleProps } from "./MediaBubbles";
import { mediaKey } from "./media-hub";
import { whenVisible } from "./near-viewport";
import { bubbleBox, saveNameFor } from "./visual-media";

type LoadStatus = "idle" | "loading" | "ready" | "error";

function thumbKey(props: MediaBubbleProps): string | null {
  const t = props.attachment.thumb;
  return t === undefined || t.blobId.length === 0 ? null : `thumb:${mediaKey(t.blobId)}`;
}

function boxStyle(props: MediaBubbleProps): JSX.CSSProperties {
  const box = bubbleBox(props.attachment, props.attachment.thumb);
  return {
    width: `${box.width}px`,
    "aspect-ratio": `${box.width} / ${box.height}`,
  };
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

/** Full-screen viewer: black ground, close button, Save. Escape or a click on
 *  the ground closes it. */
function MediaViewer(props: {
  kind: "photo" | "video";
  url: string;
  saveName: string;
  onClose: () => void;
  onVideo?: (el: HTMLVideoElement) => void;
}): JSX.Element {
  let closeButton: HTMLButtonElement | undefined;
  const onKey = (e: KeyboardEvent): void => {
    if (e.key === "Escape") props.onClose();
  };
  onMount(() => {
    globalThis.document?.addEventListener("keydown", onKey);
    closeButton?.focus();
  });
  onCleanup(() => globalThis.document?.removeEventListener("keydown", onKey));
  return (
    <Portal>
      <div
        class="lk-viewer"
        role="dialog"
        aria-modal="true"
        aria-label={props.kind === "photo" ? t("chat.media.photo") : t("chat.media.video")}
        onClick={(e) => {
          if (e.target === e.currentTarget) props.onClose();
        }}
      >
        <div class="lk-viewer-bar">
          <button type="button" onClick={() => saveObjectUrl(props.url, props.saveName)}>
            Save
          </button>
          <button ref={closeButton} type="button" aria-label={t("common.close")} onClick={() => props.onClose()}>
            Close
          </button>
        </div>
        <Show
          when={props.kind === "video"}
          fallback={<img src={props.url} alt={t("chat.media.photo")} draggable={false} />}
        >
          <video
            ref={(el) => props.onVideo?.(el)}
            src={props.url}
            controls
            autoplay
            playsinline
          />
        </Show>
      </div>
    </Portal>
  );
}

// ----- photo ---------------------------------------------------------------

export function PhotoBubble(props: MediaBubbleProps): JSX.Element {
  const [thumbUrl, setThumbUrl] = createSignal<string | null>(null);
  const [fullUrl, setFullUrl] = createSignal<string | null>(null);
  const [status, setStatus] = createSignal<LoadStatus>("idle");
  const [failure, setFailure] = createSignal<AttachmentFailure>("failed");
  const [viewer, setViewer] = createSignal(false);
  let root: HTMLButtonElement | undefined;
  const hasRemote = (): boolean => props.attachment.blobId.length > 0;

  function adoptCached(): boolean {
    const full = props.hub.urlFor(props.mediaKey);
    if (full !== null) {
      setFullUrl(full);
      setStatus("ready");
      return true;
    }
    const tk = thumbKey(props);
    const thumb = tk === null ? null : props.hub.urlFor(tk);
    if (thumb !== null) setThumbUrl(thumb);
    return false;
  }

  async function load(): Promise<void> {
    if (adoptCached()) return;
    if (!hasRemote()) return;
    setStatus("loading");
    const thumb = props.attachment.thumb;
    if (thumb !== undefined && thumbUrl() === null) {
      // A failed thumb is not a failed photo: the full image may still load.
      void props.hub.loadThumb(thumb).then(
        (u) => {
          if (u !== null && fullUrl() === null) setThumbUrl(u);
        },
        () => undefined,
      );
    }
    try {
      setFullUrl(await props.hub.load(props.attachment));
      setStatus("ready");
    } catch (err) {
      setFailure(openFailureOf(err));
      setStatus("error");
    }
  }

  onMount(() => void adoptCached());
  whenVisible(() => root, () => void load());
  // An outgoing photo's local bytes can land after the bubble mounted.
  createEffect(
    on(
      () => props.hub.urlFor(props.mediaKey),
      (u) => {
        if (u !== null && fullUrl() === null) {
          setFullUrl(u);
          setStatus("ready");
        }
      },
      { defer: true },
    ),
  );

  const label = (): string => {
    if (status() === "error" && failure() === "gone") return t("chat.attachment.gone.title");
    if (status() === "error" && failure() === "limit") return t("chat.attachment.limit.open");
    if (status() === "error") return t("chat.photo.load_failed_retry");
    if (status() === "loading") return t("chat.photo.loading");
    return t("chat.media.photo");
  };

  return (
    <>
      <button
        ref={root}
        type="button"
        class="lk-visual"
        style={boxStyle(props)}
        aria-label={label()}
        onClick={() => {
          // A gone file offers no retry (section 4.1).
          if (status() === "error") {
            if (failure() !== "gone") void load();
          } else if (fullUrl() !== null) setViewer(true);
        }}
      >
        <Show
          when={fullUrl()}
          fallback={
            <Show when={thumbUrl()}>
              {(u) => <img class="thumb" src={u()} alt="" draggable={false} />}
            </Show>
          }
        >
          {(u) => <img src={u()} alt="" draggable={false} />}
        </Show>
        <Show when={status() === "loading" || typeof props.upload === "number"}>
          <span class="lk-visual-corner" aria-hidden="true">
            <span class="lakko-spinner" />
          </span>
        </Show>
        <Show when={status() === "error"}>
          <span class="lk-visual-center">
            <span class="lk-visual-note">
              <AttachmentFailureNote failure={failure()} inline>
                {t("chat.photo.load_failed")}
              </AttachmentFailureNote>
            </span>
          </span>
        </Show>
      </button>
      <Show when={viewer() && fullUrl() !== null}>
        <MediaViewer
          kind="photo"
          url={fullUrl() ?? ""}
          saveName={saveNameFor(props.attachment, "photo")}
          onClose={() => setViewer(false)}
        />
      </Show>
    </>
  );
}

// ----- video ---------------------------------------------------------------

export function VideoBubble(props: MediaBubbleProps): JSX.Element {
  // What a cleanup releases. Reading `props.mediaKey` there would run the
  // parent's getter while the parent is being torn down, where it can throw
  // and stop the route change that disposed this bubble.
  const settledKey = createMemo(() => props.mediaKey);
  const [posterUrl, setPosterUrl] = createSignal<string | null>(null);
  const [videoUrl, setVideoUrl] = createSignal<string | null>(null);
  const [status, setStatus] = createSignal<LoadStatus>("idle");
  const [failure, setFailure] = createSignal<AttachmentFailure>("failed");
  const [viewer, setViewer] = createSignal(false);
  let root: HTMLButtonElement | undefined;
  let player: HTMLVideoElement | undefined;

  onMount(() => {
    const tk = thumbKey(props);
    const cached = tk === null ? null : props.hub.urlFor(tk);
    if (cached !== null) setPosterUrl(cached);
    const local = props.hub.urlFor(props.mediaKey);
    if (local !== null) setVideoUrl(local);
  });
  // Only the poster loads on its own; the video waits for a tap.
  whenVisible(
    () => root,
    () => {
      const thumb = props.attachment.thumb;
      if (thumb === undefined || posterUrl() !== null) return;
      void props.hub.loadThumb(thumb).then(
        (u) => setPosterUrl(u),
        () => undefined,
      );
    },
  );

  async function open(): Promise<void> {
    if (typeof props.upload === "number") return;
    const have = videoUrl() ?? props.hub.urlFor(props.mediaKey);
    if (have !== null) {
      setVideoUrl(have);
      setViewer(true);
      return;
    }
    if (props.attachment.blobId.length === 0 || status() === "loading") return;
    // A gone file offers no retry (section 4.1).
    if (status() === "error" && failure() === "gone") return;
    setStatus("loading");
    try {
      setVideoUrl(await props.hub.load(props.attachment));
      setStatus("idle");
      setViewer(true);
    } catch (err) {
      setFailure(openFailureOf(err));
      setStatus("error");
    }
  }

  function closeViewer(): void {
    player?.pause();
    props.hub.release(props.mediaKey);
    player = undefined;
    setViewer(false);
  }
  onCleanup(() => {
    player?.pause();
    props.hub.release(settledKey());
  });

  const size = (): string => formatAttachmentSize(props.attachment.size);
  const label = (): string =>
    status() === "error" && failure() === "gone"
      ? t("chat.attachment.gone.title")
      : status() === "error" && failure() === "limit"
        ? t("chat.attachment.limit.open")
        : status() === "error"
      ? t("chat.video.load_failed_retry")
      : status() === "loading"
        ? t("chat.video.loading", { size: size() })
        : t("chat.video.play", { size: size() });

  return (
    <>
      <button
        ref={root}
        type="button"
        class="lk-visual"
        style={boxStyle(props)}
        aria-label={label()}
        onClick={() => void open()}
      >
        <Show when={posterUrl()}>{(u) => <img src={u()} alt="" draggable={false} />}</Show>
        <span class="lk-visual-center" aria-hidden="true">
          <span class="lk-visual-play">
            <Show when={status() === "loading" || typeof props.upload === "number"} fallback={<span>▶</span>}>
              <span class="lakko-spinner" />
            </Show>
          </span>
          <Show when={status() === "error"}>
            <span class="lk-visual-note">
              <AttachmentFailureNote failure={failure()} inline>
                {t("chat.video.load_failed")}
              </AttachmentFailureNote>
            </span>
          </Show>
        </span>
        <Show when={props.attachment.size > 0}>
          <span class="lk-visual-size" aria-hidden="true">
            {size()}
          </span>
        </Show>
      </button>
      <Show when={viewer() && videoUrl() !== null}>
        <MediaViewer
          kind="video"
          url={videoUrl() ?? ""}
          saveName={saveNameFor(props.attachment, "video")}
          onClose={closeViewer}
          onVideo={(el) => {
            player = el;
            props.hub.claim(props.mediaKey, () => el.pause());
          }}
        />
      </Show>
    </>
  );
}
