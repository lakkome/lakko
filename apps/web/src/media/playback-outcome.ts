// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 / UX spec §2, §3 — what a voice or video-note bubble does when
 * starting playback does not go as planned.
 *
 * "Couldn't play this message" is reserved for the two cases the spec names:
 * a failed download / decrypt, or a real media error on the element. A
 * rejected `play()` promise is almost never either of those:
 *
 *   * `AbortError` — the load was interrupted (the element was reset, its
 *     source changed, or `pause()` raced the play). Benign: retry once, then
 *     fall back to a plain "tap to play" state, never the error copy.
 *   * `NotAllowedError` — the browser refused playback with sound, typically
 *     because the user activation expired while the first tap downloaded and
 *     decrypted the media. A muted start is allowed without activation, so a
 *     video note falls back to muted inline playback with an unmute
 *     affordance; a voice message (sound is the whole message) returns to its
 *     play button.
 *   * `NotSupportedError` — the source cannot be played at all. That is a real
 *     media error.
 *
 * Everything here is pure (or takes a minimal element interface) so it runs
 * under vitest with no DOM.
 */

/** What the bubble does after a rejected `play()`. */
export type PlayRejectionOutcome =
  /** Try `play()` again once. */
  | "retry"
  /** Start muted inline playback and offer "Tap for sound". */
  | "muted-fallback"
  /** Stay (or return) to the idle poster / play button, no error copy. */
  | "tap-to-play"
  /** Show "Couldn't play this message". */
  | "error";

export interface PlayRejectionContext {
  /** How many retries have already been made for this start. */
  readonly retries: number;
  /** Whether the rejected attempt was already muted. */
  readonly muted: boolean;
  /** Whether a muted fallback makes sense for this media (video: yes). */
  readonly canPlayMuted: boolean;
}

/** The benign-retry budget for an `AbortError`. */
export const MAX_ABORT_RETRIES = 1;

/** The name of a rejection reason, or `undefined` when it carries none. */
export function errorName(err: unknown): string | undefined {
  if (typeof err === "object" && err !== null && "name" in err) {
    const name = (err as { name: unknown }).name;
    return typeof name === "string" ? name : undefined;
  }
  return undefined;
}

export function playRejectionOutcome(name: string | undefined, ctx: PlayRejectionContext): PlayRejectionOutcome {
  switch (name) {
    case "AbortError":
      return ctx.retries < MAX_ABORT_RETRIES ? "retry" : "tap-to-play";
    case "NotAllowedError":
      return ctx.canPlayMuted && !ctx.muted ? "muted-fallback" : "tap-to-play";
    case "NotSupportedError":
      return "error";
    default:
      // An unnamed or unexpected rejection is not evidence of a broken
      // message; a real media error also arrives as the element's `error`
      // event, which `mediaErrorIsReal` judges.
      return "tap-to-play";
  }
}

/** `MediaError.MEDIA_ERR_ABORTED` — the fetch was aborted by the user agent
 *  or the page (e.g. the source was cleared). Not a broken message. */
const MEDIA_ERR_ABORTED = 1;

/** Whether an element `error` event reflects a real media failure (network,
 *  decode, unsupported source) rather than an aborted load. */
export function mediaErrorIsReal(code: number | undefined): boolean {
  return code !== undefined && code !== MEDIA_ERR_ABORTED;
}

/** The part of an `HTMLMediaElement` the readiness wait needs. */
export interface ReadinessTarget {
  readonly readyState: number;
  readonly error: { readonly code: number } | null;
  addEventListener(type: string, listener: () => void): void;
  removeEventListener(type: string, listener: () => void): void;
}

/** `HTMLMediaElement.HAVE_METADATA`. */
export const HAVE_METADATA = 1;

export type ReadinessResult = "ready" | "timeout" | "error";

/**
 * Resolve once the element has at least its metadata (duration, dimensions),
 * so `currentTime` can be set and `play()` does not race a fresh load.
 * Resolves `"timeout"` after `timeoutMs` (the caller may still try `play()`,
 * which waits for data itself) and `"error"` on a real media error.
 */
export function waitForMetadata(
  el: ReadinessTarget,
  timeoutMs: number,
  timers: {
    setTimeout: (fn: () => void, ms: number) => unknown;
    clearTimeout: (handle: unknown) => void;
  } = {
    setTimeout: (fn, ms) => globalThis.setTimeout(fn, ms),
    clearTimeout: (h) => globalThis.clearTimeout(h as ReturnType<typeof setTimeout>),
  },
): Promise<ReadinessResult> {
  if (el.readyState >= HAVE_METADATA) return Promise.resolve("ready");
  if (el.error !== null && mediaErrorIsReal(el.error.code)) return Promise.resolve("error");
  return new Promise((resolve) => {
    let done = false;
    const finish = (result: ReadinessResult): void => {
      if (done) return;
      done = true;
      el.removeEventListener("loadedmetadata", onReady);
      el.removeEventListener("canplay", onReady);
      el.removeEventListener("error", onError);
      timers.clearTimeout(handle);
      resolve(result);
    };
    const onReady = (): void => finish("ready");
    const onError = (): void => {
      if (el.error !== null && mediaErrorIsReal(el.error.code)) finish("error");
    };
    el.addEventListener("loadedmetadata", onReady);
    el.addEventListener("canplay", onReady);
    el.addEventListener("error", onError);
    const handle = timers.setTimeout(() => finish("timeout"), timeoutMs);
  });
}
