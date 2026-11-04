// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 — the browser glue around `recording-machine.ts`.
 *
 * Translates pointer, keyboard, timer and capture callbacks into machine
 * events, and performs the effects the machine returns. It decides nothing on
 * its own: every "does this send / lock / cancel / discard" answer comes from
 * the reducer, which is where the tests are.
 *
 * Owns at most one capture at a time and releases every stream and object URL
 * it created on `dispose` (called from the owning component's `onCleanup`).
 */

import { t } from "../l10n";
import { createSignal, type Accessor } from "solid-js";

import { recordingUnsupported } from "./capture-format";
import {
  AcquireError,
  MediaTake,
  acquireStream,
  hasCamera,
  queryPermission,
  recordingSupported,
  stopStream,
  type CaptureResult,
  type PermissionState,
} from "./media-capture";
import { recordMode, setRecordMode } from "./media-prefs";
import { QUIET_WATCH, observeLevel, type SilenceWatch } from "./microphone-silence";
import {
  HOLD_TO_RECORD_MS,
  IDLE,
  isCapturing,
  reduce,
  type CaptureKind,
  type PointerKind,
  type RecEffect,
  type RecEvent,
  type RecHint,
  type RecState,
} from "./recording-machine";

const TICK_MS = 100;
const HINT_MS = 2500;

export type RecorderNotice = RecHint | "no-camera" | "start-failed";

const NOTICE_KEYS: Readonly<Record<RecorderNotice, string>> = {
  "hold-to-record": "chat.record.hint_hold",
  cancelled: "chat.record.cancelled",
  "no-camera": "chat.record.no_camera",
  "start-failed": "chat.record.start_failed",
};

/** The notice's text in the current language (ADR-0061). */
export const noticeCopy = (notice: RecorderNotice): string => t(NOTICE_KEYS[notice]);

/** UX spec §6 — the denied card, web wording. */
export const deniedCopy = (kind: CaptureKind): string =>
  kind === "voice" ? t("chat.record.denied_microphone") : t("chat.record.denied_camera");

export const IN_CALL_COPY = (): string => t("chat.record.in_call");

export interface PreviewTake {
  readonly result: CaptureResult;
  readonly url: string;
}

export interface Recorder {
  readonly state: Accessor<RecState>;
  readonly elapsedMs: Accessor<number>;
  readonly level: Accessor<number>;
  /** UX spec section 2 "Hearing nothing" - the microphone has given nothing for
   *  two seconds and the bar should say so. Never true before a meter has
   *  measured anything, so an absent meter accuses nobody. */
  readonly micSilent: Accessor<boolean>;
  readonly notice: Accessor<RecorderNotice | null>;
  readonly denied: Accessor<CaptureKind | null>;
  readonly preview: Accessor<PreviewTake | null>;
  /** The live stream a video note's round preview shows. */
  readonly liveStream: Accessor<MediaStream | null>;
  readonly facing: Accessor<"user" | "environment">;
  /** Why the record button is disabled, or `null` when it is usable. */
  readonly disabledReason: Accessor<string | null>;
  readonly mode: Accessor<CaptureKind>;

  onPointerDown(e: PointerEvent): void;
  onPointerMove(e: PointerEvent): void;
  onPointerUp(e: PointerEvent): void;
  onPointerCancel(): void;
  /** A click with `detail === 0`: keyboard or assistive-technology activation. */
  onKeyboardActivate(): void;
  toggleMode(): void;
  stop(): void;
  remove(): void;
  cancel(): void;
  sendPreview(): void;
  flipCamera(): void;
  dismissDenied(): void;
  dispose(): void;
}

export interface RecorderOptions {
  /** A finished take the user chose to send. */
  onSend(result: CaptureResult): void;
  /** `true` while a call owns the microphone and camera. */
  inCall: Accessor<boolean>;
  /** Registers object URLs this controller creates, so they are revoked. */
  createUrl(blob: Blob): string | null;
  revokeUrl(url: string): void;
}

export function createRecorder(opts: RecorderOptions): Recorder {
  const [state, setState] = createSignal<RecState>(IDLE);
  const [elapsedMs, setElapsed] = createSignal(0);
  const [level, setLevel] = createSignal(0);
  const [micSilent, setMicSilent] = createSignal(false);
  // One run per take, folded forward in the ticker. Plain state rather than a
  // signal: the fold's inputs are read at the same 100 ms tick that writes it,
  // so nothing else ever needs to observe it changing.
  let silence: SilenceWatch = QUIET_WATCH;
  const [notice, setNotice] = createSignal<RecorderNotice | null>(null);
  const [denied, setDenied] = createSignal<CaptureKind | null>(null);
  const [preview, setPreview] = createSignal<PreviewTake | null>(null);
  const [liveStream, setLiveStream] = createSignal<MediaStream | null>(null);
  const [facing, setFacing] = createSignal<"user" | "environment">("user");
  const [cameraPresent, setCameraPresent] = createSignal<boolean | null>(null);

  const supported: Record<CaptureKind, boolean> = {
    voice: recordingSupported("voice"),
    video_note: recordingSupported("video_note"),
  };
  const permission: Record<CaptureKind, PermissionState> = { voice: "unknown", video_note: "unknown" };

  let take: MediaTake | null = null;
  /** A stream a permission request produced, not yet handed to a take. */
  let pendingStream: MediaStream | null = null;
  /** Bumped whenever a capture is abandoned, so a late async acquire can tell
   *  it arrived for a take that no longer exists. */
  let generation = 0;
  let ticker: ReturnType<typeof setInterval> | null = null;
  /** The one timer that tells a press it has become a hold (UX spec §3.1). */
  let holdTimer: ReturnType<typeof setTimeout> | null = null;
  let noticeTimer: ReturnType<typeof setTimeout> | null = null;
  let disposed = false;

  const refreshPermissions = (): void => {
    for (const kind of ["voice", "video_note"] as const) {
      void queryPermission(kind).then((p) => {
        permission[kind] = p;
      });
    }
  };
  refreshPermissions();
  void hasCamera().then(setCameraPresent);

  const onKey = (e: KeyboardEvent): void => {
    if (e.key === "Escape" && state().phase !== "idle") {
      e.preventDefault();
      dispatch({ type: "cancel" });
    }
  };
  globalThis.document?.addEventListener("keydown", onKey);
  // A permission prompt takes the focus, and the pointer-up may then land
  // outside the window: losing focus while the stream is awaited counts as
  // the pointer coming up, so a grant after a prompt never starts a take.
  const onWindowBlur = (): void => {
    if (state().phase === "requesting") dispatch({ type: "release", at: Date.now() });
  };
  globalThis.addEventListener?.("blur", onWindowBlur);

  const mode = (): CaptureKind => (cameraPresent() === false ? "voice" : recordMode());

  const disabledReason = (): string | null => {
    if (opts.inCall()) return IN_CALL_COPY();
    if (!supported[mode()]) return recordingUnsupported(mode());
    return null;
  };

  function showNotice(n: RecorderNotice): void {
    setNotice(n);
    if (noticeTimer !== null) clearTimeout(noticeTimer);
    noticeTimer = setTimeout(() => setNotice(null), HINT_MS);
  }

  function startTicker(): void {
    if (ticker !== null) return;
    ticker = setInterval(() => {
      const s = state();
      if (s.phase === "recording" || s.phase === "locked") {
        const elapsed = Math.max(0, Date.now() - s.startedAt);
        setElapsed(elapsed);
        setLevel(take?.level() ?? 0);
        // Only a voice take is judged: the spec's rule is in section 2, and a
        // video note's bar has no line to say it in. `null` is a take whose
        // meter has measured nothing at all, which is not the same as a meter
        // reporting zero and must not raise the warning.
        const measured = mode() === "voice" ? (take?.measuredLevel() ?? null) : null;
        if (measured !== null) {
          silence = observeLevel(silence, measured, elapsed);
          setMicSilent(silence.warns);
        }
        dispatch({ type: "tick", at: Date.now() });
      } else {
        stopTicker();
      }
    }, TICK_MS);
  }

  function stopTicker(): void {
    if (ticker !== null) clearInterval(ticker);
    ticker = null;
    setLevel(0);
  }

  /**
   * Arms (or disarms) the hold threshold, UX spec §3.1.
   *
   * A press starts nothing; the microphone opens only when this fires, so the
   * whole difference between a tap that switches the mode and a hold that
   * records is one timeout. It is armed ONCE per press - a `move` produces a
   * fresh `pressing` state, and re-arming there would let a drifting finger
   * push the moment away indefinitely.
   *
   * The machine re-checks the elapsed time itself, so a late timer (a
   * throttled tab) cannot open a microphone on a press that is already over.
   */
  function armHold(next: RecState): void {
    if (next.phase === "pressing") {
      if (holdTimer !== null) return;
      const remaining = Math.max(0, HOLD_TO_RECORD_MS - (Date.now() - next.pressedAt));
      holdTimer = setTimeout(() => {
        holdTimer = null;
        dispatch({ type: "tick", at: Date.now() });
      }, remaining);
      return;
    }
    if (holdTimer !== null) {
      clearTimeout(holdTimer);
      holdTimer = null;
    }
  }

  function dispatch(event: RecEvent): void {
    if (disposed) return;
    const next = reduce(state(), event);
    setState(next.state);
    for (const effect of next.effects) perform(effect);
    if (isCapturing(next.state)) startTicker();
    armHold(next.state);
    // Whatever the machine did with it, a stream nobody claimed is released.
    if (pendingStream !== null && !isCapturing(next.state) && next.state.phase !== "requesting") {
      stopStream(pendingStream);
      pendingStream = null;
    }
  }

  function beginTake(kind: CaptureKind, stream: MediaStream): void {
    const t = new MediaTake(kind, stream);
    t.onTrackEnded(() => dispatch({ type: "revoked" }));
    try {
      t.start();
    } catch {
      t.discard();
      showNotice("start-failed");
      dispatch({ type: "capture-failed" });
      return;
    }
    take = t;
    // A fresh take is owed a fresh verdict; a warning earned by the last one
    // would otherwise open this one.
    silence = QUIET_WATCH;
    setMicSilent(false);
    setFacing(t.facingMode);
    setLiveStream(kind === "video_note" ? t.previewStream : null);
  }

  function perform(effect: RecEffect): void {
    switch (effect.type) {
      case "request-permission": {
        // A fresh attempt starts here, so the counters do too. Without this
        // the "Starting..." bar shows the PREVIOUS take's length until the
        // stream lands, which reads as a recording already several seconds
        // in - the same lie, one state earlier.
        setElapsed(0);
        setLevel(0);
        const gen = generation;
        acquireStream(effect.kind).then(
          (stream) => {
            if (disposed || gen !== generation || state().phase !== "requesting") {
              stopStream(stream);
              return;
            }
            pendingStream = stream;
            permission[effect.kind] = "granted";
            dispatch({ type: "permission-result", granted: true, at: Date.now() });
          },
          (err: unknown) => {
            if (gen !== generation || state().phase !== "requesting") return;
            const reason = err instanceof AcquireError ? err.reason : "failed";
            if (reason === "denied") {
              permission[effect.kind] = "denied";
              dispatch({ type: "permission-result", granted: false, at: Date.now() });
            } else {
              dispatch({ type: "cancel" });
              showNotice(reason === "no-device" && effect.kind === "video_note" ? "no-camera" : "start-failed");
            }
          },
        );
        return;
      }
      /**
       * SYNCHRONOUS, and that is the invariant rather than an implementation
       * detail: nothing is awaited between the machine entering a capturing
       * phase and `MediaRecorder.start()`.
       *
       * THE DEFECT this shape exists for: this used to call `acquireStream`
       * and begin the take when the promise resolved, while the machine had
       * already entered `recording` and the ticker had already started. On the
       * common path — permission granted, so the machine went straight to
       * `recording` without passing through `requesting` — the recording bar
       * ran, the clock counted and the red dot pulsed for as long as the
       * browser took to open the audio device, and everything said into that
       * window was lost. The fix is upstream, in the machine: the stream is
       * acquired in `requesting`, and a capturing phase is only ever reached
       * on the event that hands the stream over. All that is left here is to
       * take it.
       */
      case "start-capture": {
        setDenied(null);
        setElapsed(0);
        const stream = pendingStream;
        pendingStream = null;
        if (stream === null) {
          // Unreachable through the machine, which never asks for a capture
          // except on the grant that produced a stream. If it ever happens,
          // the honest answer is a recorder that did not start - never one
          // that starts late, which is the defect above.
          showNotice("start-failed");
          dispatch({ type: "capture-failed" });
          return;
        }
        beginTake(effect.kind, stream);
        return;
      }
      case "release-stream":
        stopStream(pendingStream);
        pendingStream = null;
        return;
      case "finish": {
        generation += 1;
        stopTicker();
        const t = take;
        take = null;
        setLiveStream(null);
        if (t === null) {
          // The capture never got going (a release before the stream arrived).
          if (effect.disposition === "preview") dispatch({ type: "preview-failed" });
          return;
        }
        if (effect.disposition === "discard") {
          t.discard();
          return;
        }
        void t.stop().then(
          (result) => {
            if (disposed) return;
            if (effect.disposition === "send") {
              opts.onSend(result);
              return;
            }
            const url = opts.createUrl(result.blob);
            if (url === null) return;
            setPreview({ result, url });
            dispatch({ type: "preview-ready", durationMs: result.durationMs });
          },
          () => {
            showNotice("start-failed");
            if (effect.disposition === "preview") dispatch({ type: "preview-failed" });
          },
        );
        return;
      }
      case "hint":
        showNotice(effect.hint);
        return;
      case "show-denied":
        setDenied(effect.kind);
        return;
      case "toggle-mode":
        toggleMode();
        return;
      case "send-preview": {
        const p = preview();
        setPreview(null);
        if (p !== null) {
          opts.revokeUrl(p.url);
          opts.onSend(p.result);
        }
        return;
      }
      case "discard-preview": {
        const p = preview();
        setPreview(null);
        if (p !== null) opts.revokeUrl(p.url);
        return;
      }
    }
  }

  function pointerKind(e: PointerEvent): PointerKind {
    return e.pointerType === "touch" || e.pointerType === "pen" ? e.pointerType : "mouse";
  }

  function toggleMode(): void {
    if (cameraPresent() === false) {
      showNotice("no-camera");
      return;
    }
    setRecordMode(recordMode() === "voice" ? "video_note" : "voice");
  }

  return {
    state,
    elapsedMs,
    level,
    micSilent,
    notice,
    denied,
    preview,
    liveStream,
    facing,
    disabledReason,
    mode,
    onPointerDown(e) {
      if (e.button !== 0 || disabledReason() !== null) return;
      e.preventDefault();
      try {
        (e.currentTarget as Element | null)?.setPointerCapture(e.pointerId);
      } catch {
        // Capture is an improvement, not a requirement.
      }
      dispatch({
        type: "press",
        kind: mode(),
        pointer: pointerKind(e),
        x: e.clientX,
        y: e.clientY,
        at: Date.now(),
        permission: permission[mode()],
      });
    },
    onPointerMove(e) {
      // `pressing` too: the drift it collects decides whether the release is
      // still a tap, and it is the offset a promoted take inherits.
      // `requesting` too: the device takes time to open, the finger does not
      // stop for it, and the cancel and lock targets are answered from that
      // offset the moment the stream lands.
      const phase = state().phase;
      if (phase === "recording" || phase === "pressing" || phase === "requesting") {
        dispatch({ type: "move", x: e.clientX, y: e.clientY });
      }
    },
    onPointerUp() {
      dispatch({ type: "release", at: Date.now() });
    },
    onPointerCancel() {
      const phase = state().phase;
      if (phase === "recording" || phase === "requesting" || phase === "pressing") {
        dispatch({ type: "cancel" });
      }
    },
    onKeyboardActivate() {
      if (disabledReason() !== null || state().phase !== "idle") return;
      dispatch({ type: "activate", kind: mode(), at: Date.now(), permission: permission[mode()] });
    },
    toggleMode,
    stop: () => dispatch({ type: "stop", at: Date.now() }),
    remove: () => dispatch({ type: "delete" }),
    cancel: () => dispatch({ type: "cancel" }),
    sendPreview: () => dispatch({ type: "send" }),
    flipCamera() {
      const t = take;
      if (t === null) return;
      void t.flipCamera().then((ok) => {
        if (ok) {
          setFacing(t.facingMode);
          setLiveStream(null);
          setLiveStream(t.previewStream);
        }
      });
    },
    dismissDenied: () => {
      setDenied(null);
      refreshPermissions();
    },
    dispose() {
      if (disposed) return;
      dispatch({ type: "cancel" });
      const p = preview();
      if (p !== null) opts.revokeUrl(p.url);
      disposed = true;
      generation += 1;
      stopTicker();
      if (holdTimer !== null) clearTimeout(holdTimer);
      take?.discard();
      take = null;
      stopStream(pendingStream);
      pendingStream = null;
      if (noticeTimer !== null) clearTimeout(noticeTimer);
      globalThis.document?.removeEventListener("keydown", onKey);
      globalThis.removeEventListener?.("blur", onWindowBlur);
    },
  };
}
