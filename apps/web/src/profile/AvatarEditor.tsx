// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * docs/ux/profile-screen.md section 2 — the photo's menu, the crop screen and
 * Lakko's own camera. Markup and browser glue only: the arithmetic is in
 * `avatar-crop.ts`, the encoding in `avatar-media.ts`.
 */
import { createSignal, For, onCleanup, onMount, Show, type JSX } from "solid-js";

import { t } from "../l10n";
import { Icon } from "../ui/icons";
import { ModalLayer } from "../ui/ModalLayer";
import { acquireStream, hasCamera, stopStream } from "../media/media-capture";
import {
  INITIAL_CROP,
  clampVideoStart,
  cropScale,
  dragCrop,
  maxVideoStartMs,
  sourceSquare,
  zoomCrop,
  MAX_ZOOM,
  MIN_ZOOM,
  type CropState,
} from "./avatar-crop";
import { VIDEO_MAX_DURATION_MS } from "./avatar-model";
import {
  avatarVideoMime,
  encodeAvatarVideo,
  encodeStill,
  loadImage,
  loadVideo,
  seekTo,
  videoStripFrames,
  type LocalAvatar,
} from "./avatar-media";

/** What goes into the crop screen. */
export type CropSource = { readonly kind: "image"; readonly blob: Blob } | { readonly kind: "video"; readonly blob: Blob };

// ---------------------------------------------------------------------------
// The menu (section 2.1)
// ---------------------------------------------------------------------------

export function AvatarMenu(props: {
  readonly canRemove: boolean;
  readonly onPick: (source: CropSource) => void;
  readonly onCamera: () => void;
  readonly onRemove: () => void;
  readonly onClose: () => void;
  readonly onUnusable: () => void;
}): JSX.Element {
  let input: HTMLInputElement | undefined;
  const [camera, setCamera] = createSignal(false);
  onMount(() => {
    void hasCamera().then((v) => setCamera(v === true));
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === "Escape") props.onClose();
    };
    document.addEventListener("keydown", onKey);
    onCleanup(() => document.removeEventListener("keydown", onKey));
  });
  return (
    <>
      <div class="lk-menu-backdrop" onClick={() => props.onClose()} />
      <div class="lk-menu lk-avatar-menu" role="menu" aria-label={t("profile.avatar.change")}>
        <button type="button" role="menuitem" onClick={() => input?.click()}>
          <Icon name="clip" size={16} />
          {t("profile.avatar.menu.library")}
        </button>
        <Show when={camera()}>
          <button
            type="button"
            role="menuitem"
            onClick={() => {
              props.onCamera();
            }}
          >
            <Icon name="camera" size={16} />
            {t("profile.avatar.menu.camera")}
          </button>
        </Show>
        <Show when={props.canRemove}>
          <button type="button" role="menuitem" class="danger-item" onClick={() => props.onRemove()}>
            <Icon name="trash" size={16} />
            {t("profile.avatar.menu.remove")}
          </button>
        </Show>
        <input
          ref={input}
          type="file"
          accept="image/*,video/*"
          style={{ display: "none" }}
          onChange={(e) => {
            const file = e.currentTarget.files?.[0];
            e.currentTarget.value = "";
            if (file === undefined) return;
            if (file.type.startsWith("image/")) props.onPick({ kind: "image", blob: file });
            else if (file.type === "video/mp4") props.onPick({ kind: "video", blob: file });
            else props.onUnusable();
          }}
        />
      </div>
    </>
  );
}

// ---------------------------------------------------------------------------
// The crop screen (section 2.2)
// ---------------------------------------------------------------------------

const VIEWPORT = 280;
const STRIP_FRAMES = 8;

export function AvatarCrop(props: {
  readonly source: CropSource;
  readonly onCancel: () => void;
  readonly onUse: (avatar: LocalAvatar) => void;
}): JSX.Element {
  const [dims, setDims] = createSignal<{ w: number; h: number } | null>(null);
  const [crop, setCrop] = createSignal<CropState>(INITIAL_CROP);
  const [url, setUrl] = createSignal<string | null>(null);
  const [startMs, setStartMs] = createSignal(0);
  const [durationMs, setDurationMs] = createSignal(0);
  const [strip, setStrip] = createSignal<string[]>([]);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  let image: HTMLImageElement | null = null;
  let video: HTMLVideoElement | null = null;
  let videoUrl: string | null = null;
  let previewVideo: HTMLVideoElement | undefined;

  onMount(() => {
    void (async () => {
      try {
        if (props.source.kind === "image") {
          image = await loadImage(props.source.blob);
          const u = URL.createObjectURL(props.source.blob);
          setUrl(u);
          setDims({ w: image.naturalWidth, h: image.naturalHeight });
        } else {
          const loaded = await loadVideo(props.source.blob);
          video = loaded.video;
          videoUrl = loaded.url;
          setUrl(loaded.url);
          setDims({ w: video.videoWidth, h: video.videoHeight });
          setDurationMs(video.duration * 1000);
          await seekTo(video, 0);
          setStrip(await videoStripFrames(video, STRIP_FRAMES));
        }
      } catch {
        setError(t("profile.video.unusable"));
      }
    })();
  });
  onCleanup(() => {
    const u = url();
    if (u !== null && u !== videoUrl) URL.revokeObjectURL(u);
    if (videoUrl !== null) URL.revokeObjectURL(videoUrl);
  });

  const scale = (): number => {
    const d = dims();
    return d === null ? 1 : cropScale(d.w, d.h, VIEWPORT, crop().zoom);
  };
  const pointers = new Map<number, { x: number; y: number }>();
  let pinchStart: { distance: number; zoom: number } | null = null;
  const distance = (): number => {
    const [a, b] = [...pointers.values()];
    return a === undefined || b === undefined ? 0 : Math.hypot(a.x - b.x, a.y - b.y);
  };

  function onPointerDown(e: PointerEvent): void {
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (pointers.size === 2) pinchStart = { distance: distance(), zoom: crop().zoom };
  }
  function onPointerMove(e: PointerEvent): void {
    const prev = pointers.get(e.pointerId);
    const d = dims();
    if (prev === undefined || d === null) return;
    pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (pointers.size >= 2 && pinchStart !== null && pinchStart.distance > 0) {
      setCrop((c) => zoomCrop(d.w, d.h, VIEWPORT, c, (pinchStart!.zoom * distance()) / pinchStart!.distance));
      return;
    }
    setCrop((c) => dragCrop(d.w, d.h, VIEWPORT, c, e.clientX - prev.x, e.clientY - prev.y));
  }
  function onPointerUp(e: PointerEvent): void {
    pointers.delete(e.pointerId);
    if (pointers.size < 2) pinchStart = null;
  }
  function onWheel(e: WheelEvent): void {
    const d = dims();
    if (d === null) return;
    e.preventDefault();
    setCrop((c) => zoomCrop(d.w, d.h, VIEWPORT, c, c.zoom * (1 - e.deltaY * 0.0015)));
  }
  function setZoom(z: number): void {
    const d = dims();
    if (d !== null) setCrop((c) => zoomCrop(d.w, d.h, VIEWPORT, c, z));
  }
  function setStart(ms: number): void {
    const s = clampVideoStart(ms, durationMs());
    setStartMs(s);
    if (previewVideo !== undefined) void seekTo(previewVideo, s);
  }

  async function onUse(): Promise<void> {
    const d = dims();
    if (d === null || busy()) return;
    setBusy(true);
    setError(null);
    try {
      const square = sourceSquare(d.w, d.h, VIEWPORT, crop());
      if (props.source.kind === "image" && image !== null) {
        const still = await encodeStill(image, square);
        if (still === null) {
          setError(t("profile.save.error.generic"));
          return;
        }
        props.onUse({
          still: still.blob,
          stillSide: still.side,
          video: null,
          stillUrl: URL.createObjectURL(still.blob),
          videoUrl: null,
        });
        return;
      }
      if (video !== null) {
        const out = await encodeAvatarVideo(video, props.source.blob, square, startMs());
        if (out.kind === "ok") props.onUse(out.avatar);
        else setError(t("profile.video.unusable"));
      }
    } catch {
      setError(props.source.kind === "video" ? t("profile.video.unusable") : t("profile.save.error.generic"));
    } finally {
      setBusy(false);
    }
  }

  const mediaStyle = (): JSX.CSSProperties => {
    const d = dims();
    if (d === null) return {};
    const s = scale();
    const c = crop();
    return {
      width: `${d.w * s}px`,
      height: `${d.h * s}px`,
      transform: `translate(calc(-50% + ${c.panX}px), calc(-50% + ${c.panY}px))`,
    };
  };

  return (
    <ModalLayer open={true} title={t("profile.crop.title")} onClose={() => props.onCancel()}>
      <div class="lk-avatar-crop lk-stack">
        <div
          class="lk-avatar-crop-stage"
          style={{ width: `${VIEWPORT}px`, height: `${VIEWPORT}px` }}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerUp}
          onWheel={onWheel}
        >
          <Show when={url()}>
            {(u) =>
              props.source.kind === "image" ? (
                <img class="lk-avatar-crop-media" src={u()} alt="" draggable={false} style={mediaStyle()} />
              ) : (
                <video
                  ref={previewVideo}
                  class="lk-avatar-crop-media"
                  src={u()}
                  muted
                  playsinline
                  preload="auto"
                  style={mediaStyle()}
                />
              )
            }
          </Show>
          <div class="lk-avatar-crop-mask" aria-hidden="true" />
        </div>
        <label class="lk-avatar-crop-zoom">
          <input
            type="range"
            min={MIN_ZOOM}
            max={MAX_ZOOM}
            step={0.01}
            value={crop().zoom}
            aria-label={t("profile.crop.zoom")}
            onInput={(e) => setZoom(Number(e.currentTarget.value))}
          />
        </label>
        <Show when={props.source.kind === "video" && durationMs() > VIDEO_MAX_DURATION_MS}>
          <div class="lk-avatar-strip">
            <div class="lk-avatar-strip-frames" aria-hidden="true">
              <For each={strip()}>{(f) => <img src={f} alt="" />}</For>
              <div
                class="lk-avatar-strip-window"
                style={{
                  left: `${(startMs() / durationMs()) * 100}%`,
                  width: `${(VIDEO_MAX_DURATION_MS / durationMs()) * 100}%`,
                }}
              />
            </div>
            <input
              type="range"
              min={0}
              max={maxVideoStartMs(durationMs())}
              step={100}
              value={startMs()}
              aria-label={t("profile.crop.start")}
              onInput={(e) => setStart(Number(e.currentTarget.value))}
            />
          </div>
        </Show>
        <Show when={error()}>
          {(msg) => (
            <div class="form-error" role="alert">
              {msg()}
            </div>
          )}
        </Show>
        <div class="lk-actions">
          <button type="button" class="primary" disabled={busy() || dims() === null} onClick={() => void onUse()}>
            {busy() ? t("profile.save.saving") : t("profile.crop.use")}
          </button>
          <button type="button" class="ghost" disabled={busy()} onClick={() => props.onCancel()}>
            {t("profile.crop.cancel")}
          </button>
        </div>
      </div>
    </ModalLayer>
  );
}

// ---------------------------------------------------------------------------
// The camera (section 2.3)
// ---------------------------------------------------------------------------

const RING_RADIUS = 34;
const RING_LENGTH = 2 * Math.PI * RING_RADIUS;

export function AvatarCamera(props: {
  readonly onClose: () => void;
  readonly onCaptured: (source: CropSource) => void;
}): JSX.Element {
  const videoMime = avatarVideoMime();
  const [mode, setMode] = createSignal<"photo" | "video">("photo");
  const [facing, setFacing] = createSignal<"user" | "environment">("user");
  const [stream, setStream] = createSignal<MediaStream | null>(null);
  const [denied, setDenied] = createSignal(false);
  const [recording, setRecording] = createSignal(false);
  const [elapsed, setElapsed] = createSignal(0);
  let preview: HTMLVideoElement | undefined;
  let recorder: MediaRecorder | null = null;
  let chunks: Blob[] = [];
  let tick: ReturnType<typeof setInterval> | null = null;
  let closed = false;

  async function open(f: "user" | "environment"): Promise<void> {
    stopStream(stream());
    setStream(null);
    try {
      const s = await acquireStream("video_note", f, { audio: false, idealSide: 1280 });
      if (closed) {
        stopStream(s);
        return;
      }
      setStream(s);
      if (preview !== undefined) {
        preview.srcObject = s;
        void preview.play().catch(() => undefined);
      }
    } catch {
      // Denied, no camera, or the camera failed: the same sentence (2.3).
      setDenied(true);
    }
  }
  onMount(() => void open(facing()));
  onCleanup(() => {
    closed = true;
    if (tick !== null) clearInterval(tick);
    try {
      recorder?.stop();
    } catch {
      /* already stopped */
    }
    stopStream(stream());
  });

  function takePhoto(): void {
    const v = preview;
    if (v === undefined || v.videoWidth === 0) return;
    const canvas = document.createElement("canvas");
    canvas.width = v.videoWidth;
    canvas.height = v.videoHeight;
    canvas.getContext("2d")?.drawImage(v, 0, 0);
    canvas.toBlob((blob) => {
      if (blob !== null) props.onCaptured({ kind: "image", blob });
    }, "image/png");
  }

  function stopVideo(): void {
    if (tick !== null) clearInterval(tick);
    tick = null;
    setRecording(false);
    try {
      recorder?.stop();
    } catch {
      /* already stopped */
    }
  }

  function startVideo(): void {
    const s = stream();
    if (s === null || videoMime === null) return;
    chunks = [];
    const r = new MediaRecorder(new MediaStream(s.getVideoTracks()), { mimeType: videoMime });
    r.ondataavailable = (e): void => {
      if (e.data.size > 0) chunks.push(e.data);
    };
    r.onstop = (): void => {
      if (closed) return;
      const blob = new Blob(chunks, { type: "video/mp4" });
      if (blob.size > 0) props.onCaptured({ kind: "video", blob });
    };
    recorder = r;
    r.start(500);
    setRecording(true);
    const startedAt = performance.now();
    setElapsed(0);
    tick = setInterval(() => {
      const ms = performance.now() - startedAt;
      setElapsed(Math.min(ms, VIDEO_MAX_DURATION_MS));
      if (ms >= VIDEO_MAX_DURATION_MS) stopVideo();
    }, 50);
  }

  function onShutter(): void {
    if (mode() === "photo") takePhoto();
    else if (recording()) stopVideo();
    else startVideo();
  }

  return (
    <ModalLayer open={true} title={t("profile.avatar.menu.camera")} onClose={() => props.onClose()}>
      <Show
        when={!denied()}
        fallback={
          <div class="lk-stack">
            <p role="alert">{t("profile.camera.denied")}</p>
            <div class="lk-actions">
              <button type="button" class="primary" onClick={() => props.onClose()}>
                {t("profile.camera.close")}
              </button>
            </div>
          </div>
        }
      >
        <div class="lk-avatar-camera lk-stack">
          <div class="lk-avatar-camera-view">
            <video
              ref={preview}
              muted
              autoplay
              playsinline
              classList={{ mirrored: facing() === "user" }}
            />
          </div>
          <Show when={videoMime !== null}>
            <div class="lk-segmented lk-avatar-mode" role="radiogroup">
              <button
                type="button"
                role="radio"
                aria-checked={mode() === "photo"}
                classList={{ active: mode() === "photo" }}
                disabled={recording()}
                onClick={() => setMode("photo")}
              >
                {t("profile.camera.photo")}
              </button>
              <button
                type="button"
                role="radio"
                aria-checked={mode() === "video"}
                classList={{ active: mode() === "video" }}
                disabled={recording()}
                onClick={() => setMode("video")}
              >
                {t("profile.camera.video")}
              </button>
            </div>
          </Show>
          <div class="lk-avatar-camera-controls">
            <span class="lk-avatar-camera-spacer" />
            <button
              type="button"
              class="lk-avatar-shutter"
              classList={{ recording: recording(), video: mode() === "video" }}
              aria-label={t("profile.camera.shutter")}
              disabled={stream() === null}
              onClick={onShutter}
            >
              <svg viewBox="0 0 80 80" width="80" height="80" aria-hidden="true">
                <circle cx="40" cy="40" r={RING_RADIUS} class="lk-avatar-shutter-track" />
                <Show when={recording()}>
                  <circle
                    cx="40"
                    cy="40"
                    r={RING_RADIUS}
                    class="lk-avatar-shutter-ring"
                    stroke-dasharray={`${RING_LENGTH}`}
                    stroke-dashoffset={`${RING_LENGTH * (1 - elapsed() / VIDEO_MAX_DURATION_MS)}`}
                  />
                </Show>
              </svg>
            </button>
            <button
              type="button"
              class="lk-icon-button"
              title={t("profile.camera.switch")}
              aria-label={t("profile.camera.switch")}
              disabled={recording()}
              onClick={() => {
                const next = facing() === "user" ? "environment" : "user";
                setFacing(next);
                void open(next);
              }}
            >
              <Icon name="cameraFlip" size={22} />
            </button>
          </div>
        </div>
      </Show>
    </ModalLayer>
  );
}

// ---------------------------------------------------------------------------
// The leave dialog (section 6)
// ---------------------------------------------------------------------------

export function UnsavedChangesDialog(props: {
  readonly open: boolean;
  readonly busy: boolean;
  readonly onSave: () => void;
  readonly onDiscard: () => void;
  readonly onCancel: () => void;
}): JSX.Element {
  return (
    <ModalLayer open={props.open} title={t("profile.unsaved.title")} onClose={() => props.onCancel()}>
      <p>{t("profile.unsaved.body")}</p>
      <div class="lk-actions">
        <button type="button" class="primary" disabled={props.busy} onClick={() => props.onSave()}>
          {props.busy ? t("profile.save.saving") : t("profile.unsaved.save")}
        </button>
        <button type="button" disabled={props.busy} onClick={() => props.onDiscard()}>
          {t("profile.unsaved.discard")}
        </button>
        <button type="button" class="ghost" disabled={props.busy} onClick={() => props.onCancel()}>
          {t("profile.unsaved.cancel")}
        </button>
      </div>
    </ModalLayer>
  );
}
