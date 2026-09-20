// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Joining a call with what the device has (`docs/ux/calls.md` section 4.4).
 *
 * A video call asked the browser for a camera and a microphone in one
 * request, so a device without a camera — or a browser that blocks capture —
 * failed the whole request and the person never got into the call; the
 * landing then printed the browser's own words (`Invalid constraint`,
 * `Permission denied`). The rule now: capture what is there, join anyway, and
 * say in one plain line what is missing.
 *
 *   camera + microphone  -> camera + microphone
 *   camera fails         -> microphone only, camera off  ("camera")
 *   microphone fails too -> no capture, listen and watch ("camera-and-microphone")
 *
 * The crypto gate is untouched: media is still SFrame-sealed before it
 * leaves, and a person who publishes nothing seals nothing. This module is
 * pure so the fallback order and the copy are unit-tested; the session hands
 * it `getUserMedia`.
 */

/** Why a capture request failed, as the browser's error name says. */
export type CaptureFailureReason = "not-found" | "not-allowed" | "other";

/** What the person joined without. */
export interface CaptureFallback {
  readonly missing: "camera" | "camera-and-microphone";
  /** The reason of the LAST failure (the microphone's when both failed). */
  readonly reason: CaptureFailureReason;
}

/** What was captured, and what was given up. */
export interface CaptureOutcome<Stream> {
  /** `null` when nothing could be captured. */
  readonly stream: Stream | null;
  /** `null` when everything asked for was captured. */
  readonly fallback: CaptureFallback | null;
}

/** Map a `getUserMedia` rejection to a reason. The names are the ones the
 *  WebRTC spec and the older browsers use; anything else is `other`. */
export function captureFailureReason(err: unknown): CaptureFailureReason {
  const name =
    typeof err === "object" && err !== null && "name" in err
      ? String((err as { name: unknown }).name)
      : "";
  switch (name) {
    case "NotFoundError":
    case "DevicesNotFoundError":
    case "OverconstrainedError":
    case "ConstraintNotSatisfiedError":
      return "not-found";
    case "NotAllowedError":
    case "PermissionDeniedError":
    case "SecurityError":
      return "not-allowed";
    default:
      return "other";
  }
}

/**
 * Capture with the fallback order above. `capture(audio, video)` is the
 * browser call; it resolves the stream or rejects. Never rejects itself: a
 * total failure is an outcome, not an error, because the person still joins.
 */
export async function captureWithFallback<Stream>(
  wanted: { readonly audio: boolean; readonly video: boolean },
  capture: (audio: boolean, video: boolean) => Promise<Stream>,
): Promise<CaptureOutcome<Stream>> {
  if (!wanted.audio && !wanted.video) return { stream: null, fallback: null };
  try {
    return { stream: await capture(wanted.audio, wanted.video), fallback: null };
  } catch (first) {
    if (wanted.video && wanted.audio) {
      try {
        return {
          stream: await capture(true, false),
          fallback: { missing: "camera", reason: captureFailureReason(first) },
        };
      } catch (second) {
        return {
          stream: null,
          fallback: { missing: "camera-and-microphone", reason: captureFailureReason(second) },
        };
      }
    }
    return {
      stream: null,
      fallback: {
        missing: wanted.video ? "camera" : "camera-and-microphone",
        reason: captureFailureReason(first),
      },
    };
  }
}

/** The one line the call shows for a fallback. Plain words, what happened and
 *  what the person has now; never the browser's error text. */
export function captureFallbackNotice(fallback: CaptureFallback): string {
  if (fallback.missing === "camera") {
    switch (fallback.reason) {
      case "not-found":
        return "No camera was found. You joined with your microphone.";
      case "not-allowed":
        return "Lakko was not allowed to use the camera. You joined with your microphone.";
      default:
        return "The camera could not be started. You joined with your microphone.";
    }
  }
  switch (fallback.reason) {
    case "not-found":
      return "No microphone or camera was found. You can listen and watch.";
    case "not-allowed":
      return "Lakko was not allowed to use the microphone or camera. You can listen and watch.";
    default:
      return "The microphone and camera could not be started. You can listen and watch.";
  }
}
