// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 / UX spec §2, §3, §5 — the timeline bubbles for voice messages,
 * video notes and GIFs.
 *
 * A bubble is chosen by `hub.decision(ref)` — the sender's `kind` AND an
 * allowlisted MIME type of the matching media class — before this file is
 * reached; nothing here re-decides it. Media is fetched and decrypted through
 * the hub, whose AEAD open fails before any byte is handed to an element, so
 * a bubble either plays authenticated bytes or shows "Couldn't play".
 */

import { createEffect, createSignal, For, on, onCleanup, onMount, Show, type JSX } from "solid-js";

import type { AttachmentRefBytes } from "../chat/attachments";
import { autoplayVideoNotes } from "./media-prefs";
import { ProgressRing } from "./MediaComposer";
import type { MediaHub } from "./media-hub";
import { nextPlaybackRate, rateLabel } from "./playback-queue";
import { formatClock, spokenDuration } from "./recording-machine";
import { PhotoBubble, VideoBubble } from "./VisualBubbles";
import { waveformBars } from "./waveform";

/** Upload state of an outgoing media message: a completed fraction while
 *  uploading, `"failed"` when nothing reached the other side, `undefined`
 *  once sent (or for a received message). */
export type MediaUpload = number | "failed" | undefined;

export interface MediaBubbleProps {
  /** Not named `ref`: Solid's compiler turns a `ref` prop on a component
   *  into a ref-callback, so the object would never arrive. */
  attachment: AttachmentRefBytes;
  hub: MediaHub;
  /** The key the media is filed under: hex blob id, or a local key for an
   *  outgoing take that has not been uploaded yet. */
  mediaKey: string;
  incoming: boolean;
  upload: MediaUpload;
  onRetry: () => void;
}

const WAVE_BARS = 40;
const SEEK_STEP_MS = 5000;

function NotSent(props: { onRetry: () => void }): JSX.Element {
  return (
    <button
      type="button"
      onClick={() => props.onRetry()}
      style={{
        border: "none",
        background: "transparent",
        color: "inherit",
        padding: 0,
        "font-size": "0.78rem",
        "text-decoration": "underline",
      }}
    >
      Not sent · Retry
    </button>
  );
}

// ----- voice ---------------------------------------------------------------

export function VoiceBubble(props: MediaBubbleProps): JSX.Element {
  const [status, setStatus] = createSignal<"idle" | "loading" | "error">("idle");
  const [playing, setPlaying] = createSignal(false);
  const [position, setPosition] = createSignal(0);
  const [mediaDuration, setMediaDuration] = createSignal<number | null>(null);
  const [rate, setRate] = createSignal(1);
  let audio: HTMLAudioElement | null = null;

  const duration = (): number => props.attachment.durationMs ?? mediaDuration() ?? 0;
  const bars = (): number[] => waveformBars(props.attachment.waveform, WAVE_BARS);
  const uploading = (): boolean => typeof props.upload === "number";
  const unplayed = (): boolean => props.incoming && !props.hub.isPlayed(props.mediaKey);
  // The play button is a filled circle in the bubble's text colour; its glyph
  // takes the colour that contrasts with that in each bubble tone and theme.
  const glyphColour = (): string => (props.incoming ? "var(--surface-elevated)" : "var(--accent)");

  async function ensureAudio(): Promise<HTMLAudioElement | null> {
    if (audio !== null) return audio;
    setStatus("loading");
    try {
      const url = props.hub.urlFor(props.mediaKey) ?? (await props.hub.load(props.attachment));
      const a = new Audio(url);
      a.preload = "auto";
      a.addEventListener("timeupdate", () => setPosition(a.currentTime * 1000));
      a.addEventListener("loadedmetadata", () => {
        if (Number.isFinite(a.duration)) setMediaDuration(Math.round(a.duration * 1000));
      });
      a.addEventListener("play", () => setPlaying(true));
      a.addEventListener("pause", () => setPlaying(false));
      a.addEventListener("ended", () => {
        setPlaying(false);
        setPosition(0);
        props.hub.voiceEnded(props.mediaKey);
      });
      a.addEventListener("error", () => {
        setPlaying(false);
        setStatus("error");
      });
      audio = a;
      setStatus("idle");
      return a;
    } catch {
      setStatus("error");
      return null;
    }
  }

  async function play(): Promise<void> {
    const a = await ensureAudio();
    if (a === null) return;
    props.hub.claim(props.mediaKey, () => a.pause());
    a.playbackRate = rate();
    try {
      await a.play();
      props.hub.markPlayed(props.mediaKey);
    } catch {
      setStatus("error");
    }
  }

  function toggle(): void {
    if (uploading()) return;
    if (playing()) audio?.pause();
    else void play();
  }

  async function seekTo(ms: number): Promise<void> {
    const a = await ensureAudio();
    if (a === null) return;
    const clamped = Math.max(0, Math.min(duration() || ms, ms));
    a.currentTime = clamped / 1000;
    setPosition(clamped);
  }

  createEffect(
    on(props.hub.requestedPlay, (key) => {
      if (key === props.mediaKey) {
        props.hub.consumeRequest(key);
        void play();
      }
    }),
  );

  onCleanup(() => {
    if (audio !== null) {
      audio.pause();
      props.hub.release(props.mediaKey);
      audio.removeAttribute("src");
      audio = null;
    }
  });

  let wave: HTMLSpanElement | undefined;
  function seekFromPointer(e: PointerEvent): void {
    if (wave === undefined || duration() === 0 || uploading()) return;
    const rect = wave.getBoundingClientRect();
    const fraction = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
    void seekTo(fraction * duration());
  }

  return (
    <div class="lakko-voice">
      <button
        type="button"
        class="lakko-voice-play"
        aria-label={
          playing()
            ? "Pause voice message"
            : `Play voice message, ${spokenDuration(duration())}`
        }
        disabled={uploading()}
        onClick={toggle}
      >
        <Show when={status() !== "loading"} fallback={<span class="lakko-spinner glyph" style={{ color: glyphColour() }} />}>
          <span class="glyph" aria-hidden="true" style={{ color: glyphColour() }}>
            {playing() ? "❚❚" : "▶"}
          </span>
        </Show>
        <Show when={uploading()}>
          <ProgressRing fraction={typeof props.upload === "number" ? props.upload : 0} stroke={glyphColour()} width={8} />
        </Show>
      </button>
      <div style={{ flex: 1, display: "flex", "flex-direction": "column", gap: "0.1rem", "min-width": 0 }}>
        <span
          ref={wave}
          class="lakko-wave"
          role="slider"
          tabindex={uploading() ? -1 : 0}
          aria-label="Seek voice message"
          aria-valuemin={0}
          aria-valuemax={Math.round(duration() / 1000)}
          aria-valuenow={Math.round(position() / 1000)}
          aria-valuetext={`${spokenDuration(position())} of ${spokenDuration(duration())}`}
          onPointerDown={(e) => {
            try {
              e.currentTarget.setPointerCapture(e.pointerId);
            } catch {
              // Optional.
            }
            seekFromPointer(e);
          }}
          onPointerMove={(e) => {
            if (e.buttons !== 0) seekFromPointer(e);
          }}
          onKeyDown={(e) => {
            if (uploading()) return;
            if (e.key === "ArrowRight" || e.key === "ArrowUp") {
              e.preventDefault();
              void seekTo(position() + SEEK_STEP_MS);
            } else if (e.key === "ArrowLeft" || e.key === "ArrowDown") {
              e.preventDefault();
              void seekTo(position() - SEEK_STEP_MS);
            } else if (e.key === "Home") {
              e.preventDefault();
              void seekTo(0);
            } else if (e.key === "End") {
              e.preventDefault();
              void seekTo(duration());
            }
          }}
        >
          <For each={bars()}>
            {(h, i) => (
              <i
                data-on={duration() > 0 && (i() + 0.5) / WAVE_BARS <= position() / duration()}
                style={{ height: `${Math.round(h * 100)}%` }}
              />
            )}
          </For>
        </span>
        <Show when={status() === "error"}>
          <button
            type="button"
            onClick={() => {
              setStatus("idle");
              void play();
            }}
            style={{ border: "none", background: "transparent", color: "inherit", padding: 0, "font-size": "0.75rem", "text-align": "left", "text-decoration": "underline" }}
          >
            Couldn't play this message
          </button>
        </Show>
        <Show when={props.upload === "failed"}>
          <NotSent onRetry={props.onRetry} />
        </Show>
      </div>
      <div class="lakko-voice-meta">
        <span style={{ display: "inline-flex", "align-items": "center", gap: "0.25rem" }}>
          <Show when={unplayed()}>
            <span class="lakko-unplayed" aria-label="Not played" />
          </Show>
          {formatClock(playing() || position() > 0 ? position() : duration())}
        </span>
        <button
          type="button"
          aria-label={`Playback speed ${rateLabel(rate())}`}
          onClick={() => {
            const next = nextPlaybackRate(rate());
            setRate(next);
            if (audio !== null) audio.playbackRate = next;
          }}
        >
          {rateLabel(rate())}
        </button>
      </div>
    </div>
  );
}

// ----- video note ----------------------------------------------------------

export function VideoNoteBubble(props: MediaBubbleProps): JSX.Element {
  const [posterUrl, setPosterUrl] = createSignal<string | null>(null);
  const [status, setStatus] = createSignal<"idle" | "loading" | "error">("idle");
  const [mode, setMode] = createSignal<"poster" | "muted" | "sound">("poster");
  const [paused, setPaused] = createSignal(false);
  const [progress, setProgress] = createSignal(0);
  const [videoUrl, setVideoUrl] = createSignal<string | null>(null);
  let video: HTMLVideoElement | undefined;
  let root: HTMLButtonElement | undefined;
  const uploading = (): boolean => typeof props.upload === "number";

  onMount(() => {
    const thumb = props.attachment.thumb;
    if (thumb !== undefined) {
      void props.hub.loadThumb(thumb).then(
        (url) => setPosterUrl(url),
        () => setPosterUrl(null),
      );
    }
    const local = props.hub.urlFor(props.mediaKey);
    if (local !== null) setVideoUrl(local);
  });

  async function ensureUrl(): Promise<string | null> {
    const have = videoUrl() ?? props.hub.urlFor(props.mediaKey);
    if (have !== null) {
      setVideoUrl(have);
      return have;
    }
    setStatus("loading");
    try {
      const url = await props.hub.load(props.attachment);
      setVideoUrl(url);
      setStatus("idle");
      return url;
    } catch {
      setStatus("error");
      return null;
    }
  }

  async function startMuted(): Promise<void> {
    if (mode() !== "poster" || uploading()) return;
    if ((await ensureUrl()) === null) return;
    setMode("muted");
    queueMicrotask(() => {
      if (video === undefined) return;
      video.muted = true;
      video.loop = true;
      void video.play().catch(() => undefined);
    });
  }

  async function onTap(): Promise<void> {
    if (uploading()) return;
    if (mode() === "sound" && video !== undefined) {
      if (video.paused) {
        props.hub.claim(props.mediaKey, () => video?.pause());
        void video.play().catch(() => setStatus("error"));
      } else {
        video.pause();
      }
      return;
    }
    if ((await ensureUrl()) === null) return;
    setMode("sound");
    queueMicrotask(() => {
      if (video === undefined) return;
      props.hub.claim(props.mediaKey, () => video?.pause());
      video.muted = false;
      video.loop = false;
      video.currentTime = 0;
      void video.play().then(
        () => props.hub.markPlayed(props.mediaKey),
        () => setStatus("error"),
      );
    });
  }

  // UX spec §3 — autoplay muted and looping only when scrolled into view, and
  // only with the "Autoplay video notes" setting on (default off).
  onMount(() => {
    if (typeof IntersectionObserver === "undefined" || root === undefined) return;
    const observer = new IntersectionObserver((entries) => {
      const visible = entries.some((e) => e.isIntersecting);
      if (visible && autoplayVideoNotes() && mode() === "poster") void startMuted();
      if (!visible && mode() === "muted") {
        video?.pause();
      } else if (visible && mode() === "muted") {
        void video?.play().catch(() => undefined);
      }
    });
    observer.observe(root);
    onCleanup(() => observer.disconnect());
  });

  onCleanup(() => {
    video?.pause();
    props.hub.release(props.mediaKey);
  });

  const label = (): string => {
    const d = spokenDuration(props.attachment.durationMs ?? 0);
    if (mode() === "sound" && !paused()) return "Pause video message";
    return `Play video message, ${d}`;
  };

  return (
    <div style={{ display: "flex", "flex-direction": "column", "align-items": "center", gap: "0.2rem" }}>
      <button ref={root} type="button" class="lakko-vn-bubble" aria-label={label()} onClick={() => void onTap()}>
        <Show
          when={mode() !== "poster" && videoUrl() !== null}
          fallback={
            <Show when={posterUrl()} fallback={<span style={{ display: "block", width: "100%", height: "100%" }} />}>
              {(url) => <img src={url()} alt="" draggable={false} />}
            </Show>
          }
        >
          <video
            ref={video}
            src={videoUrl() ?? ""}
            playsinline
            muted={mode() === "muted"}
            poster={posterUrl() ?? undefined}
            onPlay={() => setPaused(false)}
            onPause={() => setPaused(true)}
            onTimeUpdate={(e) => {
              const v = e.currentTarget;
              if (Number.isFinite(v.duration) && v.duration > 0) setProgress(v.currentTime / v.duration);
            }}
            onEnded={() => {
              setProgress(0);
              props.hub.release(props.mediaKey);
              if (autoplayVideoNotes()) {
                setMode("poster");
                void startMuted();
              } else {
                setMode("poster");
              }
            }}
            onError={() => setStatus("error")}
          />
        </Show>
        <Show when={mode() === "sound"}>
          <ProgressRing fraction={progress()} stroke="#fff" width={3} />
        </Show>
        <Show when={uploading()}>
          <ProgressRing fraction={typeof props.upload === "number" ? props.upload : 0} width={4} />
        </Show>
        <span class="lakko-vn-glyph" aria-hidden="true">
          <Show when={status() === "loading"}>
            <span class="lakko-spinner" />
          </Show>
          <Show when={status() !== "loading" && (mode() === "poster" || (mode() === "sound" && paused()))}>▶</Show>
          <Show when={status() !== "loading" && mode() === "muted"}>
            <span style={{ position: "absolute", top: "1rem", right: "2.2rem", "font-size": "1rem" }}>🔇</span>
          </Show>
        </span>
        <Show when={props.attachment.durationMs !== undefined && mode() !== "sound"}>
          <span class="lakko-vn-corner">{formatClock(props.attachment.durationMs ?? 0)}</span>
        </Show>
      </button>
      <Show when={status() === "error"}>
        <button
          type="button"
          onClick={() => {
            setStatus("idle");
            setMode("poster");
            void onTap();
          }}
          style={{ border: "none", background: "transparent", color: "inherit", padding: 0, "font-size": "0.75rem", "text-decoration": "underline" }}
        >
          Couldn't play this message
        </button>
      </Show>
      <Show when={props.upload === "failed"}>
        <NotSent onRetry={props.onRetry} />
      </Show>
    </div>
  );
}

// ----- GIF -----------------------------------------------------------------

const GIF_MAX_WIDTH = 260;

export function GifBubble(props: MediaBubbleProps): JSX.Element {
  const [url, setUrl] = createSignal<string | null>(null);
  const [status, setStatus] = createSignal<"loading" | "ready" | "error">("loading");
  const [fullScreen, setFullScreen] = createSignal(false);
  const inline = (): "image" | "video" => (props.hub.inlineType(props.attachment.mime) === "video" ? "video" : "image");
  const uploading = (): boolean => typeof props.upload === "number";

  function load(): void {
    const local = props.hub.urlFor(props.mediaKey);
    if (local !== null) {
      setUrl(local);
      setStatus("ready");
      return;
    }
    if (props.attachment.blobId.length === 0) {
      // An outgoing GIF whose bytes are still being fetched through the proxy.
      setStatus("loading");
      return;
    }
    setStatus("loading");
    props.hub.load(props.attachment).then(
      (u) => {
        setUrl(u);
        setStatus("ready");
      },
      () => setStatus("error"),
    );
  }
  onMount(load);
  // An outgoing GIF's local bytes land after the bubble mounted.
  createEffect(
    on(
      () => props.hub.urlFor(props.mediaKey),
      (u) => {
        if (u !== null && url() === null) {
          setUrl(u);
          setStatus("ready");
        }
      },
      { defer: true },
    ),
  );

  const width = (): number => Math.min(GIF_MAX_WIDTH, props.attachment.width ?? GIF_MAX_WIDTH);
  const aspect = (): string =>
    props.attachment.width !== undefined && props.attachment.height !== undefined
      ? `${props.attachment.width} / ${props.attachment.height}`
      : "1 / 1";

  const onKey = (e: KeyboardEvent): void => {
    if (e.key === "Escape") setFullScreen(false);
  };
  createEffect(() => {
    if (fullScreen()) globalThis.document?.addEventListener("keydown", onKey);
    else globalThis.document?.removeEventListener("keydown", onKey);
  });
  onCleanup(() => globalThis.document?.removeEventListener("keydown", onKey));

  const media = (big: boolean): JSX.Element => (
    <Show when={url()}>
      {(u) =>
        inline() === "video" ? (
          <video src={u()} autoplay muted loop playsinline aria-label="GIF" style={big ? {} : undefined} />
        ) : (
          <img src={u()} alt="GIF" draggable={false} />
        )
      }
    </Show>
  );

  return (
    <div style={{ display: "flex", "flex-direction": "column", gap: "0.2rem" }}>
      <button
        type="button"
        class="lakko-gif"
        style={{ width: `${width()}px`, "aspect-ratio": aspect() }}
        aria-label="Open GIF full screen"
        disabled={status() !== "ready"}
        onClick={() => setFullScreen(true)}
      >
        {media(false)}
        <span class="lakko-gif-label">GIF</span>
        <Show when={status() === "loading" || uploading()}>
          <span class="lakko-vn-glyph" style={{ "font-size": "1rem" }}>
            <span class="lakko-spinner" />
          </span>
        </Show>
      </button>
      <Show when={status() === "error"}>
        <button
          type="button"
          onClick={load}
          style={{ border: "none", background: "transparent", color: "inherit", padding: 0, "font-size": "0.75rem", "text-decoration": "underline", "text-align": "left" }}
        >
          Couldn't load this GIF · Retry
        </button>
      </Show>
      <Show when={props.upload === "failed"}>
        <NotSent onRetry={props.onRetry} />
      </Show>
      <Show when={fullScreen()}>
        <div class="lakko-lightbox" role="dialog" aria-label="GIF" onClick={() => setFullScreen(false)}>
          {media(true)}
        </div>
      </Show>
    </div>
  );
}

/** Dispatch to the bubble `hub.decision` chose. Returns `null` for a file,
 *  which the caller renders as a chip. */
export function MediaAttachment(props: MediaBubbleProps): JSX.Element {
  const decision = (): string => props.hub.decision(props.attachment).bubble;
  return (
    <>
      <Show when={decision() === "voice"}>
        <VoiceBubble {...props} />
      </Show>
      <Show when={decision() === "video_note"}>
        <VideoNoteBubble {...props} />
      </Show>
      <Show when={decision() === "gif"}>
        <GifBubble {...props} />
      </Show>
      <Show when={decision() === "photo"}>
        <PhotoBubble {...props} />
      </Show>
      <Show when={decision() === "video"}>
        <VideoBubble {...props} />
      </Show>
    </>
  );
}
