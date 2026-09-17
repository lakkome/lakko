// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

// @ts-check

/**
 * The shape of a ringing notification, and what each of its buttons means.
 *
 * ## Why this is its own `public/*.js` ESM
 *
 * Same reason as `sw-push-decrypt.js`, whose header explains it at length:
 * `public/sw.js` is copied VERBATIM into `dist/` and is not part of Vite's
 * build graph, so it can neither import a `src/*.ts` module nor resolve a
 * hashed chunk. A sibling `public/*.js` ESM is the one shape reachable from
 * both the service worker (which reaches it through the dynamic import it
 * already performs for the decrypt core) and the vitest suite in
 * `src/sw/call-notification.test.ts`.
 *
 * Nothing here touches a browser global, a clock, or storage. Every function
 * is a pure value-to-value mapping, which is the point: the two decisions a
 * ringing notification embodies — what it looks like, and what pressing each
 * button should do — are then assertable without a service worker, a push
 * server, or a browser.
 *
 * ## The two decisions, and why they are separated the way they are
 *
 *   1. **kind to notification shape.** {@link callRingNotification} turns a
 *      decrypted offer into the `showNotification` arguments.
 *   2. **action to intent.** The map {@link callNotificationIntents} builds is
 *      stored INSIDE the notification's own `data` at push time, so the click
 *      handler performs a lookup rather than a decision. That is deliberate:
 *      `notificationclick` must reach `clients.openWindow` with as few awaits
 *      as possible or the browser withholds the user activation that permits
 *      it, and a dynamic import there could cost a network fetch on a
 *      cold-started worker. Deciding at push time — where the handler already
 *      awaits a decrypt — and reading a plain object at click time removes
 *      that risk entirely.
 *
 * ## What is NOT decided here
 *
 * Whether a device rings at all. That is `sw-push-decrypt.js`'s job: it is the
 * half that holds the decrypted journal rows, the staleness rule and the
 * offer-versus-end ordering. This module is handed the verdict.
 */

/** Action id of the Accept button. Matches `event.action` on a click. */
export const CALL_ACTION_ACCEPT = "accept";

/** Action id of the Decline button. */
export const CALL_ACTION_DECLINE = "decline";

/**
 * `event.action` is the empty string when the user clicks the notification
 * BODY rather than one of its buttons. Used verbatim as a map key so the
 * click handler never has to special-case it.
 */
export const CALL_ACTION_BODY = "";

/**
 * Prefix of a ringing notification's tag.
 *
 * The tag is keyed on the CALL id, not the group: a repeat wake for the same
 * call replaces the notification in place rather than stacking a second ring,
 * and the end wake can address exactly the ring it cancels without disturbing
 * a message notification for the same chat (which is tagged by group hex).
 */
export const CALL_NOTIFICATION_TAG_PREFIX = "lakko-call:";

/** The notification tag for one call. */
export function callNotificationTag(callIdHex) {
  return `${CALL_NOTIFICATION_TAG_PREFIX}${callIdHex}`;
}

/** `true` for a tag {@link callNotificationTag} produced. */
export function isCallNotificationTag(tag) {
  return typeof tag === "string" && tag.startsWith(CALL_NOTIFICATION_TAG_PREFIX);
}

/**
 * Vibration pattern for a ring: three pulses with gaps, in milliseconds.
 *
 * Long enough to read as a call rather than a message (whose default is a
 * single short buzz), and short enough to finish well inside the 45 s ring
 * timeout so it does not outlive the call it announces. Ignored by every
 * desktop browser and by iOS, which is fine — it is an addition to the
 * notification, never the notification itself.
 */
export const CALL_VIBRATION_PATTERN = [400, 200, 400, 200, 400];

/**
 * Build the `showNotification(title, options)` arguments for an incoming call.
 *
 * `docs/ux/calls.md` §2.2 fixes the wording: the caller's name is the title
 * and the body is exactly `Voice call` or `Video call` — the kind the CALLER
 * chose, never a guess, which is why `video` comes from the offer's own media
 * flags rather than from anything local. A caller this device cannot name
 * falls back to the product name, the same fallback the message path uses;
 * it never invents an identity.
 *
 * @param {{ callIdHex: string, groupHex: string, video: boolean, callerName?: string | null }} call
 */
export function callRingNotification(call) {
  const name =
    typeof call.callerName === "string" && call.callerName.length > 0
      ? call.callerName
      : "Lakko";
  return {
    title: name,
    options: {
      body: call.video ? "Video call" : "Voice call",
      tag: callNotificationTag(call.callIdHex),
      // A second wake for the SAME call must REPLACE the ring rather than
      // stack another one, but it must also re-alert: a ring the user has not
      // answered is still asking for an answer.
      renotify: true,
      // A ring is not a message: it stays until the user acts on it, because
      // a notification that auto-dismisses after a few seconds is a missed
      // call. The 45 s ring timeout still governs the call itself; the
      // cancelling end wake is what clears this.
      requireInteraction: true,
      vibrate: CALL_VIBRATION_PATTERN,
      icon: "/icon-192.png",
      badge: "/icon-96.png",
      actions: [
        { action: CALL_ACTION_ACCEPT, title: "Accept" },
        { action: CALL_ACTION_DECLINE, title: "Decline" },
      ],
      data: {
        call: {
          callIdHex: call.callIdHex,
          groupHex: call.groupHex,
          video: call.video,
          intents: callNotificationIntents(call),
        },
      },
    },
  };
}

/**
 * What each button on a ringing notification means, keyed by the `event.action`
 * the browser will report — including {@link CALL_ACTION_BODY} for a click on
 * the notification itself.
 *
 * Precomputed into `data` at push time; see the header for why the click
 * handler must not compute it.
 *
 * @param {{ callIdHex: string, groupHex: string, video: boolean }} call
 */
export function callNotificationIntents(call) {
  const base = { callIdHex: call.callIdHex, groupHex: call.groupHex, video: call.video };
  return {
    // A click on the body is not an answer. It raises the app and lets the
    // user see who is calling and decide — the full-screen incoming call of
    // `docs/ux/calls.md` §2.2 is already mounted app-wide, so focusing is
    // enough to show it.
    [CALL_ACTION_BODY]: { kind: "open", ...base },
    [CALL_ACTION_ACCEPT]: { kind: "accept", ...base },
    [CALL_ACTION_DECLINE]: { kind: "decline", ...base },
  };
}

/**
 * Read the intent a click carries, or `null` when the notification is not a
 * ringing one (a message notification, or one from an older build whose data
 * predates this map).
 *
 * `public/sw.js` inlines this one-line lookup rather than importing it, for
 * the user-activation reason in the header. This export is the canonical
 * definition the tests pin, and the two must stay in step — the same
 * convention `looksLikeCallSignaling` already follows between
 * `src/chat/client.ts` and `sw-push-decrypt.js`.
 */
export function intentForAction(action, data) {
  const intents = data?.call?.intents;
  if (intents === null || typeof intents !== "object") return null;
  const intent = intents[typeof action === "string" ? action : CALL_ACTION_BODY];
  if (intent === null || typeof intent !== "object") return null;
  return intent;
}

/**
 * What the click handler should actually do, given the intent and whether any
 * Lakko window exists to talk to.
 *
 * Three rules, each with a reason:
 *
 *   - **Decline never opens or raises a window.** Declining is a refusal to
 *     take the call; yanking the user into the app to complete it is the
 *     opposite of what they asked for. A page is still needed to PUBLISH the
 *     decline — the worker is read-only and cannot (C-59) — so an existing
 *     client, even a hidden or backgrounded one, is told and left alone. Only
 *     when no page exists at all does declining have to open one.
 *   - **Accept focuses or opens.** Answering a call means arriving in it.
 *   - **Open focuses or opens** without answering.
 *
 * A window that must be opened carries the intent in the URL, because a page
 * that does not exist yet cannot be sent a message.
 *
 * @param {{ kind: string, callIdHex: string, groupHex: string, video: boolean }} intent
 * @param {boolean} hasClient  Any same-origin window client exists.
 */
export function callClickPlan(intent, hasClient) {
  if (intent === null || typeof intent !== "object") {
    return { post: false, focus: false, openUrl: null };
  }
  if (hasClient) {
    return { post: true, focus: intent.kind !== "decline", openUrl: null };
  }
  return { post: false, focus: false, openUrl: callIntentUrl(intent) };
}

/**
 * The cold-open URL for an intent.
 *
 * `?wake=` is the pre-existing deep link `src/push/WakeNavigator.tsx` already
 * reads to land the user on the right chat; the call parameters ride beside it
 * and are read by `src/calls/call-notification-intent.ts`. Both are stripped
 * from the address bar on arrival, so a refresh cannot re-answer a call.
 */
export function callIntentUrl(intent) {
  const params = new URLSearchParams();
  if (typeof intent.groupHex === "string" && intent.groupHex.length > 0) {
    params.set("wake", intent.groupHex);
  }
  if (intent.kind === "accept" || intent.kind === "decline") {
    params.set("callAction", intent.kind);
    params.set("callId", intent.callIdHex);
    if (intent.video === true) params.set("callVideo", "1");
  }
  const query = params.toString();
  return query.length === 0 ? "/" : `/?${query}`;
}

/**
 * `postMessage` discriminator for a call action sent to an already-open tab.
 *
 * Deliberately distinct from `LAKKO_NOTIFICATION_TAP` (which only navigates)
 * and from `LAKKO_PUSH_WAKE` (which fires on every arrival, for every tab):
 * only an explicit press of Accept or Decline may answer or refuse a call.
 */
export const CALL_ACTION_MESSAGE = "LAKKO_CALL_ACTION";
