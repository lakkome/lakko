// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 / UX spec §2, §3, §6 — the composer's recording surfaces: the
 * record button, the recording bar that replaces the text field, the round
 * video-note preview, and the permission / hint notices.
 *
 * Pure renderers of a `Recorder` (`recording-controller.ts`); no rule lives
 * here.
 */

import { createEffect, createSignal, For, onCleanup, Show, type JSX } from "solid-js";
import { Icon } from "../ui/icons";

import { formatClock, formatTotalClock, capFor, spokenDuration, type RecState } from "./recording-machine";
import { DENIED_COPY, NOTICE_COPY, type Recorder } from "./recording-controller";
import { waveformBars } from "./waveform";

function holding(s: RecState): boolean {
  return s.phase === "recording" || s.phase === "requesting";
}

/**
 * The right-hand composer button when the field and tray are empty.
 *
 * UX spec §3.1 — ONE visible button, carrying the current mode's glyph. A tap
 * switches the mode, a hold records; both are the machine's rules, reached
 * through the pointer handlers. What sits beside it is not a second button but
 * the mode switch a keyboard or a screen reader needs, which carries no ink:
 * hold and slide are gestures those users do not have, so without it the
 * camera would be unreachable for them.
 */
export function RecordButton(props: { recorder: Recorder }): JSX.Element {
  const r = props.recorder;
  const isVideo = (): boolean => r.mode() === "video_note";
  const label = (): string => (isVideo() ? "Record video message" : "Record voice message");
  const switchLabel = (): string =>
    isVideo() ? "Switch to voice message" : "Switch to video message";
  return (
    <span
      style={{
        display: "inline-flex",
        "align-items": "stretch",
        // Hidden, never unmounted, once the take no longer needs it: the
        // button may still hold the pointer capture of the gesture that
        // locked the take.
        visibility: ["locked", "finalizing", "preview"].includes(r.state().phase) ? "hidden" : "visible",
      }}
      title={
        r.disabledReason() ??
        `${label()}: hold to record, tap to switch to ${isVideo() ? "voice" : "video"}`
      }
    >
      <button
        type="button"
        class="lk-icon-button lakko-record-btn"
        aria-label={label()}
        aria-disabled={r.disabledReason() !== null}
        disabled={r.disabledReason() !== null}
        data-active={holding(r.state())}
        onPointerDown={(e) => r.onPointerDown(e)}
        onPointerMove={(e) => r.onPointerMove(e)}
        onPointerUp={(e) => r.onPointerUp(e)}
        onPointerCancel={() => r.onPointerCancel()}
        onContextMenu={(e) => e.preventDefault()}
        onClick={(e) => {
          // A pointer press is handled by the pointer events above, where a
          // short one switches the mode instead of recording (§3.1); only a
          // keyboard / assistive-technology activation (detail 0) lands here,
          // and it starts a locked recording (UX spec §7).
          if (e.detail === 0) r.onKeyboardActivate();
        }}
      >
        <Icon name={isVideo() ? "video" : "mic"} size={20} />
      </button>
      {/* §3.1 — the second action on the same control, for the people who
          have no hold gesture. Visually hidden so the composer reads as one
          button, but in the tab order and the accessibility tree, and it does
          become visible while it holds keyboard focus: a focus stop nobody can
          see is worse than the extra glyph the spec was avoiding. */}
      <button
        type="button"
        class="lakko-visually-hidden lakko-mode-switch-a11y"
        aria-label={switchLabel()}
        // Its own tooltip, or the wrapper's would describe the recorder while
        // this is the control being pointed at once focus reveals it.
        title={switchLabel()}
        // One button, one disabled state: whatever takes the visible glyph out
        // of use - a take in progress, a call holding the microphone, a browser
        // that cannot record - takes this with it, because a mode to record in
        // is worth nothing while there is nothing to record with.
        disabled={recorderBusy(r) || r.disabledReason() !== null}
        onClick={() => r.toggleMode()}
      >
        {switchLabel()}
      </button>
    </span>
  );
}

/**
 * `true` while the recording bar replaces the text field.
 *
 * `pressing` is deliberately NOT busy (UX spec §3.1): a press that has not yet
 * lasted the hold threshold may still turn out to be a tap on the mode switch,
 * and swapping the composer for a recording bar on every tap is the flicker the
 * spec was written against.
 */
export function recorderBusy(r: Recorder): boolean {
  const s = r.state();
  return s.phase !== "idle" && s.phase !== "pressing";
}

/** Replaces the attach button and text field while a take is in progress. */
export function RecordingBar(props: { recorder: Recorder }): JSX.Element {
  const r = props.recorder;
  const s = (): RecState => r.state();
  const kindWord = (): string => {
    const st = s();
    const kind = "kind" in st ? st.kind : r.mode();
    return kind === "video_note" ? "video" : "voice";
  };
  return (
    <div class="lakko-rec-bar" role="group" aria-label={`Recording a ${kindWord()} message`}>
      <Show when={s().phase === "preview" && s()} keyed fallback={<LiveBar recorder={r} />}>
        {(p) => (
          <Show
            when={p.phase === "preview" && p.kind === "voice"}
            fallback={<span style={{ color: "var(--surface-muted)", "font-size": "0.85rem" }}>Video message ready</span>}
          >
            <VoicePreview recorder={r} />
          </Show>
        )}
      </Show>
    </div>
  );
}

function LiveBar(props: { recorder: Recorder }): JSX.Element {
  const r = props.recorder;
  const s = (): RecState => r.state();
  const dx = (): number => {
    const st = s();
    return st.phase === "recording" ? Math.min(0, st.dx) : 0;
  };
  return (
    <>
      <span class="lakko-rec-dot" aria-hidden="true" />
      <span aria-live="off">{formatClock(r.elapsedMs())}</span>
      <span class="lakko-level" aria-hidden="true">
        <span style={{ width: `${Math.round(r.level() * 100)}%` }} />
      </span>
      <Show when={s().phase === "requesting"}>
        <span style={{ color: "var(--surface-muted)", "font-size": "0.85rem" }}>Starting…</span>
      </Show>
      <Show when={s().phase === "recording"}>
        <span
          style={{
            flex: 1,
            "text-align": "center",
            color: "var(--surface-muted)",
            "font-size": "0.85rem",
            transform: `translateX(${dx() / 2}px)`,
            "white-space": "nowrap",
            overflow: "hidden",
          }}
        >
          ‹ Slide to cancel · ↑ lock
        </span>
      </Show>
      <Show when={s().phase === "locked"}>
        <span style={{ flex: 1 }} />
        <button type="button" onClick={() => r.remove()} aria-label="Delete recording">
          Delete
        </button>
        <button type="button" class="primary" onClick={() => r.stop()} aria-label="Stop recording">
          Stop
        </button>
      </Show>
      <Show when={s().phase === "finalizing"}>
        <span class="lakko-spinner" aria-label="Finishing recording" />
      </Show>
    </>
  );
}

function VoicePreview(props: { recorder: Recorder }): JSX.Element {
  const r = props.recorder;
  const [playing, setPlaying] = createSignal(false);
  const [position, setPosition] = createSignal(0);
  const audio = new Audio();
  audio.preload = "auto";
  createEffect(() => {
    const p = r.preview();
    if (p !== null && audio.src !== p.url) audio.src = p.url;
  });
  audio.addEventListener("timeupdate", () => setPosition(audio.currentTime * 1000));
  audio.addEventListener("ended", () => {
    setPlaying(false);
    setPosition(0);
  });
  audio.addEventListener("pause", () => setPlaying(false));
  audio.addEventListener("play", () => setPlaying(true));
  onCleanup(() => {
    audio.pause();
    audio.removeAttribute("src");
    audio.load();
  });
  const duration = (): number => r.preview()?.result.durationMs ?? 0;
  const bars = (): number[] => waveformBars(r.preview()?.result.waveform, 40);
  return (
    <>
      <button
        type="button"
        class="lakko-icon-btn"
        aria-label={playing() ? "Pause recording" : `Play recording, ${spokenDuration(duration())}`}
        onClick={() => (playing() ? audio.pause() : void audio.play().catch(() => undefined))}
      >
        {playing() ? "❚❚" : "▶"}
      </button>
      <span class="lakko-wave" aria-hidden="true">
        <For each={bars()}>
          {(h, i) => (
            <i
              data-on={duration() > 0 && (i() + 1) / bars().length <= position() / duration()}
              style={{ height: `${Math.round(h * 100)}%` }}
            />
          )}
        </For>
      </span>
      <span>{playing() ? formatClock(position()) : formatTotalClock(duration())}</span>
      <button type="button" onClick={() => r.remove()}>
        Delete
      </button>
      <button type="button" class="primary" onClick={() => r.sendPreview()}>
        Send
      </button>
    </>
  );
}

/** SVG progress ring drawn over a circle; `fraction` 0..1. */
export function ProgressRing(props: { fraction: number; stroke?: string; width?: number }): JSX.Element {
  const radius = 48;
  const circumference = 2 * Math.PI * radius;
  return (
    <svg class="lakko-ring" viewBox="0 0 100 100" aria-hidden="true">
      <circle
        cx="50"
        cy="50"
        r={radius}
        fill="none"
        stroke={props.stroke ?? "var(--accent)"}
        stroke-width={props.width ?? 3}
        stroke-dasharray={`${circumference}`}
        stroke-dashoffset={`${circumference * (1 - Math.max(0, Math.min(1, props.fraction)))}`}
        stroke-linecap="round"
      />
    </svg>
  );
}

/** UX spec §3 — the circular live preview over the chat while a video note
 *  is being recorded, and the looping preview once it is stopped. */
export function VideoNoteOverlay(props: { recorder: Recorder }): JSX.Element {
  const r = props.recorder;
  const visible = (): boolean => {
    const s = r.state();
    return (
      (s.phase === "recording" || s.phase === "locked" || s.phase === "finalizing" || s.phase === "preview") &&
      s.kind === "video_note"
    );
  };
  return (
    <Show when={visible()}>
      <div class="lakko-vn-overlay" data-holding={r.state().phase === "recording"}>
        <Show when={r.state().phase === "preview"} fallback={<LiveCircle recorder={r} />}>
          <PreviewCircle recorder={r} />
        </Show>
      </div>
    </Show>
  );
}

function LiveCircle(props: { recorder: Recorder }): JSX.Element {
  const r = props.recorder;
  let video: HTMLVideoElement | undefined;
  createEffect(() => {
    const stream = r.liveStream();
    if (video === undefined) return;
    video.srcObject = stream;
    if (stream !== null) void video.play().catch(() => undefined);
  });
  onCleanup(() => {
    if (video !== undefined) video.srcObject = null;
  });
  return (
    <>
      <div class="lakko-vn-circle">
        <video
          ref={video}
          muted
          playsinline
          autoplay
          style={{ transform: r.facing() === "user" ? "scaleX(-1)" : "none" }}
        />
        <ProgressRing fraction={r.elapsedMs() / capFor("video_note")} stroke="var(--danger)" width={4} />
      </div>
      <div class="lakko-vn-controls">
        <span class="lakko-rec-dot" aria-hidden="true" />
        <span>{formatClock(r.elapsedMs())}</span>
        <Show when={r.state().phase === "recording"}>
          <span style={{ color: "var(--surface-muted)", "font-size": "0.85rem" }}>
            ‹ Slide to cancel · ↑ lock
          </span>
        </Show>
        <Show when={r.state().phase === "locked"}>
          <button type="button" onClick={() => r.flipCamera()} aria-label="Flip camera">
            ⟲
          </button>
          <button type="button" onClick={() => r.remove()}>
            Delete
          </button>
          <button type="button" class="primary" onClick={() => r.stop()}>
            Stop
          </button>
        </Show>
        <Show when={r.state().phase === "finalizing"}>
          <span class="lakko-spinner" aria-label="Finishing recording" />
        </Show>
      </div>
    </>
  );
}

function PreviewCircle(props: { recorder: Recorder }): JSX.Element {
  const r = props.recorder;
  const [withSound, setWithSound] = createSignal(false);
  let video: HTMLVideoElement | undefined;
  return (
    <>
      <button
        type="button"
        class="lakko-vn-circle"
        style={{ padding: 0, border: "none" }}
        aria-label={withSound() ? "Pause video message" : "Play video message with sound"}
        onClick={() => {
          if (video === undefined) return;
          if (!withSound()) {
            setWithSound(true);
            video.muted = false;
            video.loop = false;
            video.currentTime = 0;
            void video.play().catch(() => undefined);
          } else if (video.paused) {
            void video.play().catch(() => undefined);
          } else {
            video.pause();
          }
        }}
      >
        <video
          ref={video}
          src={r.preview()?.url ?? ""}
          muted
          loop
          autoplay
          playsinline
          onEnded={() => {
            if (video === undefined) return;
            setWithSound(false);
            video.muted = true;
            video.loop = true;
            void video.play().catch(() => undefined);
          }}
        />
        <Show when={!withSound()}>
          <span class="lakko-vn-glyph" aria-hidden="true">🔇</span>
        </Show>
      </button>
      <div class="lakko-vn-controls">
        <span>{formatTotalClock(r.preview()?.result.durationMs ?? 0)}</span>
        <button type="button" onClick={() => r.remove()}>
          Delete
        </button>
        <button type="button" class="primary" onClick={() => r.sendPreview()}>
          Send
        </button>
      </div>
    </>
  );
}

/** The permission-denied card and the short hints ("Hold to record"). */
export function RecorderNotices(props: { recorder: Recorder }): JSX.Element {
  const r = props.recorder;
  return (
    <>
      <Show when={r.denied()}>
        {(kind) => (
          <div class="lakko-media-card" role="alert">
            <span style={{ flex: 1 }}>{DENIED_COPY[kind()]}</span>
            <button type="button" onClick={() => r.dismissDenied()} style={{ padding: "0.2rem 0.6rem" }}>
              Dismiss
            </button>
          </div>
        )}
      </Show>
      <div aria-live="polite" style={{ "min-height": 0 }}>
        <Show when={r.notice()}>
          {(n) => (
            <span
              class={n() === "cancelled" ? "lakko-shake" : undefined}
              style={{ display: "inline-block", "font-size": "0.8rem", color: "var(--surface-muted)" }}
            >
              {NOTICE_COPY[n()]}
            </span>
          )}
        </Show>
      </div>
    </>
  );
}
