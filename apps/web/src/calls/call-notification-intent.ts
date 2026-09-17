// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Accept and Decline pressed on a ringing notification — the PURE half.
 *
 * The service worker cannot answer or refuse a call: publishing either means
 * sealing an MLS application message, and the worker's push path is read-only
 * by construction (C-59 / ADR-0041). So a press on Accept or Decline arrives
 * in the page instead, by one of two routes, and this module turns both into
 * the same value and decides what the page should do with it. The Solid glue
 * that consumes it is `CallActionListener.tsx`.
 *
 * Splitting them is the same rule `push/wake-target.ts` follows and for the
 * same reason: `apps/web`'s vitest config runs the plain `node` environment
 * with no jsdom and no Solid transform, so a `.tsx` component is not unit
 * testable here at all. Logic in a `.ts` module is.
 *
 * ## The two routes
 *
 *   1. **A page is already open** — the worker posts
 *      `{ type: LAKKO_CALL_ACTION, intent }` to it (see
 *      `public/sw-call-notification.js`).
 *   2. **No page at all** — the worker opens
 *      `/?wake=<groupHex>&callAction=accept&callId=<hex>[&callVideo=1]`, and
 *      the parameters are read at mount and stripped immediately, so a
 *      refresh cannot answer a call a second time.
 *
 * ## Trust
 *
 * Both inputs are ATTACKER-INFLUENCEABLE: anyone can hand a user a
 * `https://lakko.me/?callAction=accept&callId=...` link, and any script on
 * the origin can post a message. What that buys an attacker is bounded to
 * nothing, by design: the ONLY effect an intent can have is to accept or
 * decline a call that is ALREADY RINGING ON THIS DEVICE with exactly that
 * call id — a call whose offer this device received over MLS, authenticated,
 * from a member of the group. An intent never creates a call, never dials,
 * never names a peer, and never reaches the network. A hostile link's worst
 * case is answering or refusing a call the user was being offered anyway, and
 * only during the seconds it is ringing.
 *
 * That is why {@link decideCallAction} matches on the call id rather than
 * simply acting on whatever is in the slot.
 */

/** `postMessage` discriminator the worker sends for a pressed call button. */
export const CALL_ACTION_MESSAGE = "LAKKO_CALL_ACTION";

/** Query parameters a cold-open call action arrives on. */
export const CALL_ACTION_PARAM = "callAction";
export const CALL_ID_PARAM = "callId";
export const CALL_VIDEO_PARAM = "callVideo";

/** A 16-byte call id is 32 hex characters. */
const CALL_ID_HEX_LEN = 32;

/** What the user pressed. */
export type CallIntentKind = "accept" | "decline" | "open";

/** A validated press of a ringing notification. */
export interface CallNotificationIntent {
  readonly kind: CallIntentKind;
  /** Lowercase hex of the 16-byte call id the notification named. */
  readonly callIdHex: string;
  /** Lowercase hex of the group, when the worker could resolve one. */
  readonly groupHex: string;
  /** The kind of call the CALLER offered, so Accept answers in kind. */
  readonly video: boolean;
}

/** What the page should do with a pending intent right now. */
export type CallActionDecision =
  /** Answer the ringing call. */
  | { readonly kind: "accept"; readonly withVideo: boolean }
  /** Refuse it. */
  | { readonly kind: "decline" }
  /** The call is not ringing here YET — keep the intent and re-decide. */
  | { readonly kind: "wait" }
  /** Nothing to do, now or ever: forget the intent. */
  | { readonly kind: "drop" };

/**
 * How long a pending intent is worth holding.
 *
 * A cold open has to wait: the page boots, the session is restored, and the
 * background receive has to poll the offer off the DS journal before anything
 * is ringing to accept. But an intent is only worth holding for as long as
 * the call it names can still be ringing, and that is the 45 s ring timeout —
 * so this is a READ of `DEFAULT_CALL_SESSION_TIMING.ringTimeoutMs`, not a
 * second timeout beside it, plus a little slack for the boot itself. Nothing
 * here changes when the ring gives up; it only stops this module from holding
 * an intent that can no longer match anything.
 */
export const CALL_ACTION_MAX_WAIT_MS = 50_000;

/** What {@link decideCallAction} needs to know about the call slot. */
export interface CallSlotView {
  /** The session has finished restoring; a `null` ringing call is now real. */
  readonly ready: boolean;
  /** The call id of the 1x1 call RINGING on this device, or `null`. */
  readonly ringingCallIdHex: string | null;
  /** Milliseconds since the intent arrived. */
  readonly waitedMs: number;
}

/**
 * Decide what to do with a pending intent.
 *
 * The rules, in order:
 *
 *   - **`open` does nothing here.** Raising the app is the worker's job and
 *     landing on the right chat is `push/wake-target.ts`'s; the full-screen
 *     incoming call is mounted app-wide, so there is nothing left to do.
 *   - **Not ready yet, or nothing ringing yet — wait.** A cold open races its
 *     own boot and the first poll of the journal.
 *   - **A DIFFERENT call is ringing — drop.** Never answer or refuse a call
 *     the user was not shown. This is the rule that makes the untrusted input
 *     harmless.
 *   - **The named call is ringing — act.**
 *   - **Waited too long — drop.** Past the ring timeout nothing can match.
 */
export function decideCallAction(
  intent: CallNotificationIntent | null,
  view: CallSlotView,
): CallActionDecision {
  if (intent === null) return { kind: "drop" };
  if (intent.kind === "open") return { kind: "drop" };
  if (view.waitedMs > CALL_ACTION_MAX_WAIT_MS) return { kind: "drop" };
  if (!view.ready) return { kind: "wait" };
  if (view.ringingCallIdHex === null) return { kind: "wait" };
  if (view.ringingCallIdHex !== intent.callIdHex) return { kind: "drop" };
  if (intent.kind === "decline") return { kind: "decline" };
  // The notification offers one Accept, so it answers the call the caller
  // actually placed: a video call is accepted with video. The full-screen
  // screen is where "Accept without video" lives (`docs/ux/calls.md` §2.2).
  return { kind: "accept", withVideo: intent.video };
}

/**
 * Read an intent out of a service-worker `message` payload, or `null` when
 * the payload is not a call action.
 *
 * Strict about the discriminator so neither `LAKKO_PUSH_WAKE` (which fires on
 * every arrival, for every tab) nor `LAKKO_NOTIFICATION_TAP` (which only
 * navigates) can ever answer a call.
 */
export function parseCallActionMessage(data: unknown): CallNotificationIntent | null {
  if (data === null || typeof data !== "object") return null;
  const msg = data as { type?: unknown; intent?: unknown };
  if (msg.type !== CALL_ACTION_MESSAGE) return null;
  return normalizeIntent(msg.intent);
}

/**
 * Read an intent out of a `location.search` string, or `null` when the query
 * carries none.
 */
export function readCallActionParams(search: string): CallNotificationIntent | null {
  let params: URLSearchParams;
  try {
    params = new URLSearchParams(search);
  } catch {
    return null;
  }
  const action = params.get(CALL_ACTION_PARAM);
  if (action === null) return null;
  return normalizeIntent({
    kind: action,
    callIdHex: params.get(CALL_ID_PARAM) ?? "",
    // The cold-open URL carries no group of its own beyond `?wake=`, which
    // `push/wake-target.ts` owns; an intent does not need one to match a
    // ringing call.
    groupHex: params.get("wake") ?? "",
    video: params.get(CALL_VIDEO_PARAM) === "1",
  });
}

/**
 * Drop the call-action parameters from an absolute href and return the
 * router-relative remainder, so a refresh cannot re-answer a call. Leaves
 * `?wake=` alone — `push/wake-target.ts` strips that one, and stripping it
 * here would race that reader.
 *
 * Returns `null` when the href does not parse, so the caller skips the
 * rewrite rather than navigating somewhere unintended.
 */
export function stripCallActionParams(href: string): string | null {
  let url: URL;
  try {
    url = new URL(href);
  } catch {
    return null;
  }
  url.searchParams.delete(CALL_ACTION_PARAM);
  url.searchParams.delete(CALL_ID_PARAM);
  url.searchParams.delete(CALL_VIDEO_PARAM);
  return `${url.pathname}${url.search}${url.hash}`;
}

/**
 * Validate an intent-shaped value from either route.
 *
 * Everything is checked because everything is attacker-supplied: an unknown
 * kind, a call id that is not exactly 32 hex characters, or a missing field
 * all yield `null` rather than a partially-trusted object.
 */
function normalizeIntent(raw: unknown): CallNotificationIntent | null {
  if (raw === null || typeof raw !== "object") return null;
  const v = raw as {
    kind?: unknown;
    callIdHex?: unknown;
    groupHex?: unknown;
    video?: unknown;
  };
  const kind = v.kind;
  if (kind !== "accept" && kind !== "decline" && kind !== "open") return null;
  if (typeof v.callIdHex !== "string") return null;
  const callIdHex = v.callIdHex.toLowerCase();
  if (callIdHex.length !== CALL_ID_HEX_LEN || !isLowerHex(callIdHex)) return null;
  const groupHex = typeof v.groupHex === "string" ? v.groupHex.toLowerCase() : "";
  return { kind, callIdHex, groupHex, video: v.video === true };
}

/** `true` when every character is a lowercase hex digit. */
function isLowerHex(s: string): boolean {
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    const digit = c >= 48 && c <= 57;
    const lower = c >= 97 && c <= 102;
    if (!digit && !lower) return false;
  }
  return true;
}
