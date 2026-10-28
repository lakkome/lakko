// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Whether this browser may send a screen share, what it is asked for, and how
 * a failed start reads (`docs/ux/screen-share.md` sections 1 and 2;
 * ADR-0083 section 6).
 *
 * Pure: every browser fact comes in as an argument, so a test states the
 * browser it means. {@link browserScreenShareEnv} reads them from the page.
 */

import { t } from "../l10n";

/** The browser facts the decisions here read. */
export interface ScreenShareEnv {
  /** `navigator.mediaDevices.getDisplayMedia` exists. */
  readonly hasGetDisplayMedia: boolean;
  /** `matchMedia("(pointer: fine)")` matches: the primary pointer is a mouse
   *  or a trackpad, not a finger. A phone or tablet browser has a coarse one. */
  readonly finePointer: boolean;
}

/**
 * Whether the `Share screen` button is there (section 1). Absent, not
 * disabled, on a browser without screen capture and on a phone or tablet.
 */
export function canSendScreenShare(env: ScreenShareEnv): boolean {
  return env.hasGetDisplayMedia && env.finePointer;
}

/** The page's own facts. Never throws; a missing API reads as "no". */
export function browserScreenShareEnv(): ScreenShareEnv {
  let hasGetDisplayMedia = false;
  let finePointer = false;
  try {
    const md = globalThis.navigator?.mediaDevices as
      | (MediaDevices & { getDisplayMedia?: unknown })
      | undefined;
    hasGetDisplayMedia = typeof md?.getDisplayMedia === "function";
  } catch {
    hasGetDisplayMedia = false;
  }
  try {
    finePointer = globalThis.matchMedia?.("(pointer: fine)").matches === true;
  } catch {
    finePointer = false;
  }
  return { hasGetDisplayMedia, finePointer };
}

/** The constraint names {@link displayMediaOptions} looks for in
 *  `getSupportedConstraints()`. */
export type DisplayAudioConstraintName = "restrictOwnAudio" | "suppressLocalAudioPlayback";

/**
 * What `getDisplayMedia` is asked for (section 2).
 *
 * The picture always; sound always asked, the browser deciding whether it
 * offers it. The call's own sound must not go back out with the share:
 *
 *   * where the browser names `restrictOwnAudio` among its supported
 *     constraints, it is asked to leave this page's own audio out of a
 *     system capture, and system audio stays allowed;
 *   * where it does not, only a tab's sound is asked for
 *     (`systemAudio: "exclude"`), since a system capture would carry the
 *     call back to the others. A browser that does not know the option
 *     ignores it.
 *
 * `suppressLocalAudioPlayback` is deliberately not used: it silences the
 * captured tab on this device, which is not what is wanted here.
 *
 * `selfBrowserSurface: "exclude"` keeps the call's own tab out of the picker
 * (the endless mirror, and the call's sound), where the browser knows the
 * option; ignored elsewhere.
 *
 * Which browsers honour which of these is not measured here; this asks and
 * the browser decides.
 */
export function displayMediaOptions(
  supported: Readonly<Partial<Record<string, boolean>>>,
): DisplayMediaStreamOptions & Record<string, unknown> {
  const restrict = supported["restrictOwnAudio"] === true;
  return {
    video: true,
    audio: restrict ? ({ restrictOwnAudio: true } as MediaTrackConstraints) : true,
    systemAudio: restrict ? "include" : "exclude",
    selfBrowserSurface: "exclude",
  };
}

/** How a start that did not give a capture reads (section 2.1). */
export type ShareStartFailure =
  /** The person closed the picker: nothing is said. */
  | "cancelled"
  /** The browser or the system refused: one line says how to allow it. */
  | "refused";

/**
 * Under this long from the press to the rejection, a `NotAllowedError` is the
 * browser refusing without showing the picker (the site's permission is
 * blocked): nobody closes a picker that fast. Chosen, not measured.
 */
export const SHARE_REFUSED_WITHIN_MS = 400;

/**
 * Read a `getDisplayMedia` rejection.
 *
 * Browsers reject a cancelled picker and a refused permission with the same
 * `NotAllowedError`, so the two are told apart by what else is known: a
 * message naming the system (Chrome's "Permission denied by system", the
 * macOS Screen Recording setting), or a rejection too quick to have been a
 * person closing the picker, is a refusal; anything else under that name is
 * a cancel. `NotFoundError`, `NotReadableError` and an unknown error are
 * refusals; `AbortError` is a cancel. This is a heuristic, stated as such.
 */
export function classifyShareStartError(err: unknown, elapsedMs: number): ShareStartFailure {
  const name = typeof err === "object" && err !== null ? String((err as { name?: unknown }).name ?? "") : "";
  const message =
    typeof err === "object" && err !== null ? String((err as { message?: unknown }).message ?? "") : "";
  if (name === "AbortError") return "cancelled";
  if (name === "NotAllowedError") {
    if (/system/i.test(message)) return "refused";
    if (elapsedMs >= 0 && elapsedMs < SHARE_REFUSED_WITHIN_MS) return "refused";
    return "cancelled";
  }
  return "refused";
}

/** Whether the page runs on macOS, where the refusal names System Settings. */
export function isMacOs(userAgent: string, platform: string | undefined): boolean {
  if (platform !== undefined && /mac/i.test(platform)) return true;
  return /Mac OS X|Macintosh/.test(userAgent) && !/iPhone|iPad|iPod/.test(userAgent);
}

/** The one line for a refused start (section 2.1). */
export function screenShareRefusedText(macOs: boolean): string {
  return macOs ? t("call.screen_share.refused_macos") : t("call.screen_share.refused");
}

/** How long a share notice stays up (section 2.1: 6 seconds). */
export const SCREEN_SHARE_NOTICE_MS = 6_000;
