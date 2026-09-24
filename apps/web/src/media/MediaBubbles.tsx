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
import { whenVisible } from "./near-viewport";
import { errorName, mediaErrorIsReal, playRejectionOutcome, waitForMetadata } from "./playback-outcome";
import { nextPlaybackRate, rateLabel } from "./playback-queue";
import { formatClock, formatTotalClock, spokenDuration } from "./recording-machine";
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

/** The affordance for a row that did not reach the server: says what happened
 *  and offers the one action, in the place the message is. Exported so the
 *  text bubble shows the SAME thing a failed media bubble does - two spellings
 *  of "this did not send" is one of them being wrong. */
export function NotSent(props: { onRetry: () => void }): JSX.Element {
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
  /** In flight: the one `prepare()` a warm-up and a press share. Without it a
   *  press landing mid-warm would start a second download of the same blob. */
  let preparing: Promise<HTMLAudioElement | null> | null = null;
  let root: HTMLDivElement | undefined;

  const duration = (): number => props.attachment.durationMs ?? mediaDuration() ?? 0;
  const bars = (): number[] => waveformBars(props.attachment.waveform, WAVE_BARS);
  const uploading = (): boolean => typeof props.upload === "number";
  const unplayed = (): boolean => props.incoming && !props.hub.isPlayed(props.mediaKey);
  // The play button is a filled circle in the bubble's text colour; its glyph
  // takes the colour that contrasts with that in each bubble tone and theme.
  const glyphColour = (): string => (props.incoming ? "var(--surface-elevated)" : "var(--accent)");

  /** The element and its listeners. Assigning `src` starts the load, so by
   *  the time this returns the browser is already buffering and demuxing. */
  function build(url: string): HTMLAudioElement {
    // `preload` is set BEFORE the source, so the element starts buffering and
    // demuxing the moment it has one rather than on the first `play()`.
    const a = new Audio();
    a.preload = "auto";
    a.src = url;
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
      // An aborted load (the source cleared on cleanup) is not a broken
      // message; only a real media error shows "Couldn't play".
      if (!mediaErrorIsReal(a.error?.code)) return;
      console.warn("[voice] media error", a.error?.code ?? "none");
      setStatus("error");
    });
    return a;
  }

  /**
   * Fetch, decrypt and build the element. Says nothing about the UI on its
   * own: it is called both from a press (where the person is waiting, so the
   * play button shows a spinner) and from the scroll-into-view warm-up (where
   * nothing is happening and a spinner would be a lie) — `forPress` is which.
   *
   * The DEFECT this exists for: all of this used to happen inside the press,
   * so a voice message's first play spent a storage round trip and an AEAD
   * open before a single sample was decoded, heard as a pause of about a
   * second between pressing play and any sound. Nothing preloaded it, though
   * the photo bubble beside it always has.
   */
  async function prepare(forPress: boolean): Promise<HTMLAudioElement | null> {
    if (audio !== null) return audio;
    // A press landing mid-warm joins the warm-up instead of starting a second
    // download of the same blob.
    if (preparing === null) {
      preparing = (async () => {
        try {
          const url = props.hub.urlFor(props.mediaKey) ?? (await props.hub.load(props.attachment));
          audio = build(url);
          return audio;
        } catch (err) {
          console.warn("[voice] download or decrypt failed", errorName(err) ?? "unnamed");
          return null;
        } finally {
          preparing = null;
        }
      })();
    }
    if (forPress) setStatus("loading");
    const a = await preparing;
    if (forPress) setStatus(a === null ? "error" : "idle");
    // A warm-up that failed says nothing and caches nothing: `audio` is still
    // null, so the press retries the fetch and is the one that reports it. A
    // background failure must not put "Couldn't play this message" under a
    // bubble nobody has touched.
    return a;
  }

  async function play(retries = 0): Promise<void> {
    const a = await prepare(true);
    if (a === null) return;
    props.hub.claim(props.mediaKey, () => a.pause());
    a.playbackRate = rate();
    try {
      await a.play();
      props.hub.markPlayed(props.mediaKey);
    } catch (err) {
      const name = errorName(err);
      // Sound is the whole message, so there is no muted fallback: a refused
      // or interrupted start returns to the play button without error copy.
      const outcome = playRejectionOutcome(name, { retries, muted: false, canPlayMuted: false });
      console.warn("[voice] play rejected", name ?? "unnamed", "outcome", outcome);
      if (outcome === "retry") {
        await play(retries + 1);
      } else if (outcome === "error") {
        setStatus("error");
      } else {
        setPlaying(false);
        props.hub.release(props.mediaKey);
      }
    }
  }

  function toggle(): void {
    if (uploading()) return;
    if (playing()) audio?.pause();
    else void play();
  }

  async function seekTo(ms: number): Promise<void> {
    const a = await prepare(true);
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

  // The fix for the pause before the sound: the bytes are fetched, decrypted
  // and handed to a buffering element while the bubble scrolls into view, so
  // a press has nothing left to wait for. This is what every other media
  // bubble already did (`near-viewport.ts`); the voice one was the outlier,
  // and it is also the one kind where the delay is the whole message.
  //
  // An outgoing take's own bytes are already filed locally, so this resolves
  // without touching the network for them.
  whenVisible(
    () => root,
    () => {
      if (props.attachment.blobId.length === 0 && props.hub.urlFor(props.mediaKey) === null) return;
      void prepare(false);
    },
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
    <div ref={root} class="lakko-voice">
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
            // Primary button only. A right-click on the waveform is a request
            // for the message's action menu (`MessageMenu.tsx`), and seeking
            // on it as well would move the playhead on the way to the menu.
            if (e.button !== 0) return;
            try {
              e.currentTarget.setPointerCapture(e.pointerId);
            } catch {
              // Optional.
            }
            seekFromPointer(e);
          }}
          onPointerMove={(e) => {
            // The primary button specifically, not "any button held".
            if ((e.buttons & 1) !== 0) seekFromPointer(e);
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
          {playing() || position() > 0 ? formatClock(position()) : formatTotalClock(duration())}
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

/** How long a start waits for the element's metadata before calling `play()`
 *  anyway (`play()` waits for data itself; this only avoids racing a fresh
 *  load when setting `currentTime`). */
const VIDEO_NOTE_READY_TIMEOUT_MS = 4000;

export function VideoNoteBubble(props: MediaBubbleProps): JSX.Element {
  const [posterUrl, setPosterUrl] = createSignal<string | null>(null);
  const [status, setStatus] = createSignal<"idle" | "loading" | "error">("idle");
  const [mode, setMode] = createSignal<"poster" | "muted" | "sound">("poster");
  const [paused, setPaused] = createSignal(true);
  const [progress, setProgress] = createSignal(0);
  const [videoUrl, setVideoUrl] = createSignal<string | null>(null);
  // Once playback has been started the <video> stays mounted — through pause,
  // end and the return to the poster — so a later tap plays an element that
  // already has its metadata instead of a brand-new one. Only a real media
  // error unmounts it, so a retry gets a fresh element.
  const [mounted, setMounted] = createSignal(false);
  // Playback with sound was refused (NotAllowedError) and the note fell back
  // to muted: offer "Tap for sound".
  const [soundHint, setSoundHint] = createSignal(false);
  let video: HTMLVideoElement | undefined;
  let root: HTMLButtonElement | undefined;
  // Bumped by every start, stop and failure: an async step that finds the
  // token moved on belongs to a superseded start and does nothing.
  let playToken = 0;
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
    } catch (err) {
      console.warn("[video-note] download or decrypt failed", errorName(err) ?? "unnamed");
      setStatus("error");
      return null;
    }
  }

  /** "Couldn't play this message": a failed download/decrypt or a real media
   *  error — nothing else reaches here. */
  function fail(): void {
    playToken++;
    video?.pause();
    props.hub.release(props.mediaKey);
    setSoundHint(false);
    setMode("poster");
    setMounted(false);
    setStatus("error");
  }

  function backToPoster(): void {
    playToken++;
    video?.pause();
    props.hub.release(props.mediaKey);
    setSoundHint(false);
    setProgress(0);
    setMode("poster");
  }

  async function playWithFallback(v: HTMLVideoElement, token: number, retries: number): Promise<void> {
    try {
      await v.play();
      if (token === playToken && !v.muted) props.hub.markPlayed(props.mediaKey);
    } catch (err) {
      if (token !== playToken) return;
      const name = errorName(err);
      const outcome = playRejectionOutcome(name, { retries, muted: v.muted, canPlayMuted: true });
      console.warn("[video-note] play rejected", name ?? "unnamed", "outcome", outcome);
      switch (outcome) {
        case "retry":
          await waitForMetadata(v, VIDEO_NOTE_READY_TIMEOUT_MS);
          if (token !== playToken) return;
          await playWithFallback(v, token, retries + 1);
          return;
        case "muted-fallback":
          props.hub.release(props.mediaKey);
          v.muted = true;
          v.loop = true;
          setMode("muted");
          setSoundHint(true);
          await playWithFallback(v, token, 0);
          return;
        case "tap-to-play":
          backToPoster();
          return;
        case "error":
          fail();
          return;
      }
    }
  }

  async function startPlayback(withSound: boolean): Promise<void> {
    const token = ++playToken;
    if ((await ensureUrl()) === null) return;
    if (token !== playToken) return;
    setMounted(true);
    // The element mounts in the reactive update above; let it settle.
    await Promise.resolve();
    const v = video;
    if (v === undefined || token !== playToken) return;
    setSoundHint(false);
    setMode(withSound ? "sound" : "muted");
    v.muted = !withSound;
    v.loop = !withSound;
    const ready = await waitForMetadata(v, VIDEO_NOTE_READY_TIMEOUT_MS);
    if (token !== playToken) return;
    if (ready === "error") {
      console.warn("[video-note] media error before playback", v.error?.code ?? "none");
      fail();
      return;
    }
    if (withSound) {
      props.hub.claim(props.mediaKey, () => backToPoster());
      if (ready === "ready") {
        try {
          v.currentTime = 0;
        } catch {
          // Seeking before data is optional; playback starts from where it is.
        }
      }
    }
    await playWithFallback(v, token, 0);
  }

  function startMuted(): void {
    if (mode() !== "poster" || uploading() || status() === "error") return;
    void startPlayback(false);
  }

  function onTap(): void {
    if (uploading()) return;
    if (mode() === "sound" && video !== undefined) {
      const v = video;
      if (v.paused) {
        props.hub.claim(props.mediaKey, () => backToPoster());
        void playWithFallback(v, playToken, 0);
      } else {
        v.pause();
      }
      return;
    }
    // Poster, or a muted loop (autoplay or the NotAllowed fallback): this tap
    // is a fresh user gesture, so play with sound from the start.
    setStatus((s) => (s === "error" ? "idle" : s));
    void startPlayback(true);
  }

  // UX spec §3 — autoplay muted and looping only when scrolled into view, and
  // only with the "Autoplay video notes" setting on (default off).
  onMount(() => {
    if (typeof IntersectionObserver === "undefined" || root === undefined) return;
    const observer = new IntersectionObserver((entries) => {
      const visible = entries.some((e) => e.isIntersecting);
      if (visible && autoplayVideoNotes() && mode() === "poster") startMuted();
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
    playToken++;
    video?.pause();
    props.hub.release(props.mediaKey);
  });

  const label = (): string => {
    const d = spokenDuration(props.attachment.durationMs ?? 0);
    if (mode() === "sound" && !paused()) return "Pause video message";
    if (mode() === "muted") return `Play video message with sound, ${d}`;
    return `Play video message, ${d}`;
  };

  return (
    <div style={{ display: "flex", "flex-direction": "column", "align-items": "center", gap: "0.2rem" }}>
      <button ref={root} type="button" class="lakko-vn-bubble" aria-label={label()} onClick={onTap}>
        <Show when={mounted() && videoUrl() !== null}>
          <video
            ref={video}
            src={videoUrl() ?? undefined}
            playsinline
            preload="auto"
            poster={posterUrl() ?? undefined}
            style={{ visibility: mode() === "poster" ? "hidden" : "visible" }}
            onPlay={() => setPaused(false)}
            onPause={() => setPaused(true)}
            onTimeUpdate={(e) => {
              const v = e.currentTarget;
              if (Number.isFinite(v.duration) && v.duration > 0) setProgress(v.currentTime / v.duration);
            }}
            onEnded={(e) => {
              if (mode() !== "sound") return;
              backToPoster();
              try {
                e.currentTarget.currentTime = 0;
              } catch {
                // Optional.
              }
              if (autoplayVideoNotes()) startMuted();
            }}
            onError={(e) => {
              const code = e.currentTarget.error?.code;
              if (!mediaErrorIsReal(code)) return;
              console.warn("[video-note] media error", code ?? "none");
              fail();
            }}
          />
        </Show>
        <Show when={mode() === "poster" || !mounted()}>
          <Show
            when={posterUrl()}
            fallback={<span style={{ position: "absolute", inset: 0, display: "block", width: "100%", height: "100%" }} />}
          >
            {(url) => <img src={url()} alt="" draggable={false} style={{ position: "absolute", inset: 0 }} />}
          </Show>
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
        <Show
          when={soundHint() && mode() === "muted"}
          fallback={
            <Show when={props.attachment.durationMs !== undefined && mode() !== "sound"}>
              <span class="lakko-vn-corner">{formatTotalClock(props.attachment.durationMs ?? 0)}</span>
            </Show>
          }
        >
          <span class="lakko-vn-corner">Tap for sound</span>
        </Show>
      </button>
      <Show when={status() === "error"}>
        <button
          type="button"
          onClick={() => {
            setStatus("idle");
            void startPlayback(true);
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
