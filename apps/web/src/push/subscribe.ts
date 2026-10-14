// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Web Push subscribe / unsubscribe orchestrator.
 *
 * Joins three layers:
 *
 *   1. **Service Worker registration** — already wired in
 *      `apps/web/src/sw/register.ts` (Phase 1.16f2). We just look
 *      up the `ServiceWorkerRegistration` and grab its
 *      `PushManager`.
 *   2. **`PushManager.subscribe({ applicationServerKey })`** — the
 *      browser-side handshake with the user's push service
 *      (FCM endpoint on Chrome, Mozilla autopush on Firefox, etc).
 *      Returns a `PushSubscription` carrying the endpoint URL plus
 *      a p256dh + auth keypair the server uses to encrypt wake
 *      payloads to us.
 *   3. **`PushClient.register`** — POST the subscription bytes to
 *      `lakko-svc-push-gw` so it knows where to fan out wakes.
 *
 * ## VAPID key shape
 *
 * `applicationServerKey` is the **public** half of the server's
 * VAPID keypair — a P-256 uncompressed point, 65 bytes, encoded
 * as base64url-no-pad. The matching private half lives on the
 * server (set via `LAKKO_WEBPUSH_VAPID_*` env vars in
 * `services/push-gw`).
 *
 * For development, generate a fresh keypair once via:
 *
 *     npx web-push generate-vapid-keys
 *
 * Stick the public half in `apps/web/.env`'s
 * `VITE_LAKKO_WEBPUSH_VAPID_PUBLIC_KEY_B64URL` and the private
 * half into the workspace `.env`'s
 * `LAKKO_WEBPUSH_VAPID_PRIVATE_KEY_B64URL` +
 * `LAKKO_WEBPUSH_VAPID_PUBLIC_KEY_B64URL`. The server uses both;
 * the client only needs the public.
 *
 * If the public key is missing at build time the subscribe flow
 * is a no-op (`subscribeToPush` returns `null`) — the rest of
 * the app keeps working, you just don't get push wakes. This is
 * the right posture for local dev where most contributors don't
 * configure VAPID right away.
 */

import { anonTokenQuotaExhausted } from "../tokens/anon-token-pool";
import { PushClient } from "./client";
import { clearDevicePushId, getOrCreateDevicePushId } from "./device-id";
import { getWakeBinding } from "./group-routing-store";
import {
  pseudonymMustRotate,
  recordWakeBinding,
  releaseWakeBinding,
} from "./wake-binding";

/** Build-time VAPID public key (base64url-no-pad). Vite inlines
 *  the `import.meta.env.VITE_*` access at build, so a missing env
 *  surfaces here as an empty string. */
const VAPID_PUBLIC_KEY_B64URL =
  (import.meta.env.VITE_LAKKO_WEBPUSH_VAPID_PUBLIC_KEY_B64URL as
    | string
    | undefined) ?? "";

/** Result of `subscribeToPush`. */
export interface SubscribeResult {
  /** The 32-byte device pseudonym we registered under. Same value
   *  you get from `getOrCreateDevicePushId()` — surfaced here so
   *  the caller can stash it without re-reading localStorage. */
  devicePseudonym: Uint8Array;
  /** The browser's `PushSubscription.endpoint` — useful for
   *  diagnostics + a "you'll be notified at <provider>" UI hint. */
  endpoint: string;
}

/**
 * Subscribe this browser to push wakes + tell `lakko-svc-push-gw`
 * about it. Idempotent: re-calling with the same SW + VAPID key
 * returns the existing subscription rather than churning a new
 * one (the browser caches the subscription per-origin).
 *
 * Returns `null` (and skips the registration) when:
 *
 *   * The build has no VAPID public key configured (local dev
 *     without `VITE_LAKKO_WEBPUSH_VAPID_PUBLIC_KEY_B64URL` set).
 *   * The browser has no Service Worker support / no Push API.
 *   * The user denied the `Notification.requestPermission()`
 *     prompt.
 *
 * The caller treats `null` as "push not available, fall back to
 * foreground polling" — no error is thrown for any of the above.
 *
 * ADR-0042, "A wake is shown only for the signed-in account": once
 * `push-gw` accepts the pseudonym, the `(pseudonym, account)` wake binding
 * is recorded for the service worker; and when the binding names another
 * account, a fresh pseudonym is minted first so that account's wakes fail
 * the worker's gate.
 *
 * The call is fire-and-forget from the sign-in paths and one of its awaits is
 * the permission prompt, which has no time limit: the person can leave it
 * open, sign out and sign in as somebody else. `isCurrent` is asked after
 * every await that precedes a write; once it answers `false` nothing is
 * rotated, registered or recorded. In particular the wake binding is never
 * recorded for an account that has signed out - the worker would treat it as
 * the signed-in one.
 *
 * @param accountIdB64 base64 account id of the account signing in.
 * @param isCurrent whether `accountIdB64` is still the signed-in account.
 */
export async function subscribeToPush(
  accountIdB64: string,
  isCurrent: () => boolean = () => true,
): Promise<SubscribeResult | null> {
  if (VAPID_PUBLIC_KEY_B64URL.length === 0) {
    // No build-time VAPID key — dev fallback.
    return null;
  }
  if (typeof navigator === "undefined" || !("serviceWorker" in navigator)) {
    return null;
  }
  if (typeof window === "undefined" || !("PushManager" in window)) {
    return null;
  }
  // Permission gate. `Notification.requestPermission()` is the
  // canonical user-facing prompt; the browser remembers the
  // answer.
  if (typeof Notification === "undefined") return null;
  if (Notification.permission === "denied") return null;
  if (Notification.permission === "default") {
    const granted = await Notification.requestPermission();
    if (granted !== "granted") return null;
    if (!isCurrent()) return null;
  }

  // The 1.16f2 register path doesn't directly expose the
  // `ServiceWorkerRegistration`, so we `ready` on the active
  // registration ourselves — by the time signup / login fires
  // we've already passed through `register.ts`.
  let reg: ServiceWorkerRegistration;
  try {
    reg = await navigator.serviceWorker.ready;
  } catch {
    return null;
  }
  if (!isCurrent()) return null;

  // Reuse an existing subscription where the browser already has
  // one — `getSubscription()` is the canonical idempotency
  // mechanism. We only call `subscribe()` on a cold cache.
  let subscription: PushSubscription | null = await reg.pushManager.getSubscription();
  if (subscription === null) {
    try {
      subscription = await reg.pushManager.subscribe({
        userVisibleOnly: true,
        // `.slice()` reifies the backing buffer as a fresh
        // `ArrayBuffer` (not `ArrayBufferLike`) so it satisfies
        // TS's `BufferSource` type — same trick the storage +
        // ds clients use elsewhere in this codebase.
        applicationServerKey:
          vapidKeyToUint8Array(VAPID_PUBLIC_KEY_B64URL).slice(),
      });
    } catch (err) {
      // Common reasons:
      //   * Quota exceeded on push service (rare for individual
      //     users; common in headless CI).
      //   * The VAPID public key on the build doesn't match what
      //     the SW was previously registered with — recovery is
      //     a manual "unsubscribe + clear caches" cycle.
      // eslint-disable-next-line no-console
      console.warn("push subscribe failed", err);
      return null;
    }
  }

  let existing = null;
  try {
    existing = await getWakeBinding();
  } catch {
    existing = null;
  }
  // Before the pseudonym is touched: rotating it for an account that has
  // left would cut the wakes of the account that is signed in now.
  if (!isCurrent()) return null;
  if (pseudonymMustRotate(existing, accountIdB64)) clearDevicePushId();
  const devicePseudonym = getOrCreateDevicePushId();

  // Serialize the subscription into the JSON shape
  // `services/push-gw/src/providers/webpush.rs::Subscription`
  // expects: `{ endpoint, keys: { p256dh, auth } }`. The base64
  // values are URL-safe-no-pad (the format `PushSubscription`
  // emits when its keys are read via `getKey()`).
  const subscriptionJson = subscriptionToJson(subscription);
  const tokenBytes = new TextEncoder().encode(subscriptionJson);

  const pushClient = new PushClient();
  try {
    await pushClient.register(devicePseudonym, "web_push", tokenBytes);
  } catch (err) {
    // ADR-0054 S3: registration spends an anonymous token. A day's allowance
    // used up is silent: the next launch registers again, and the app keeps
    // working by polling meanwhile (docs/ux/anti-abuse.md section 3).
    if (anonTokenQuotaExhausted(err) !== null) {
      // eslint-disable-next-line no-console
      console.info("push token register deferred to the next launch (token allowance used)");
      return null;
    }
    // eslint-disable-next-line no-console
    console.warn("push token register failed", err);
    return null;
  }
  // The register request spanned a sign-out. push-gw holds a row for a
  // pseudonym nothing is bound to (the worker's gate drops its wakes, and the
  // sign-out's own unregister or the next sign-in's rotation replaces it);
  // what must not happen is this account becoming the worker's signed-in one.
  if (!isCurrent()) return null;
  await recordWakeBinding(accountIdB64, devicePseudonym);

  return {
    devicePseudonym,
    endpoint: subscription.endpoint,
  };
}

/**
 * Drop the push subscription locally + tell `push-gw` to forget
 * the token. Called on logout + forget-device. Idempotent + best-
 * effort: failures are logged and swallowed (the user can't
 * really act on "push gateway is down during logout").
 */
export async function unsubscribeFromPush(): Promise<void> {
  // The callers release the wake binding before anything else; this repeat
  // is idempotent and keeps the invariant for any other caller.
  await releaseWakeBinding();
  const devicePseudonym = getOrCreateDevicePushId();
  // Best-effort server tell.
  try {
    const pushClient = new PushClient();
    await pushClient.unregister(devicePseudonym, "web_push");
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("push token unregister failed", err);
  }
  // Browser-side cancel.
  try {
    if (
      typeof navigator !== "undefined" &&
      "serviceWorker" in navigator &&
      typeof window !== "undefined" &&
      "PushManager" in window
    ) {
      const reg = await navigator.serviceWorker.getRegistration();
      const subscription = await reg?.pushManager.getSubscription();
      if (subscription !== null && subscription !== undefined) {
        await subscription.unsubscribe();
      }
    }
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("push subscribe cancel failed", err);
  }
  clearDevicePushId();
}

/** Convert a base64url-no-pad VAPID public key into the
 *  `Uint8Array` shape `PushManager.subscribe` expects. */
function vapidKeyToUint8Array(b64url: string): Uint8Array {
  // base64url-no-pad → standard-alphabet + padding so atob copes.
  const padded = b64url.replace(/-/g, "+").replace(/_/g, "/");
  const pad = padded.length % 4 === 0 ? 0 : 4 - (padded.length % 4);
  const standard = padded + "=".repeat(pad);
  const binary = atob(standard);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    out[i] = binary.charCodeAt(i);
  }
  return out;
}

/** Serialise a `PushSubscription` into the JSON shape the
 *  server expects. We don't use `subscription.toJSON()` directly
 *  because some browsers (Safari historically) omit fields — we
 *  pull each piece via the explicit getters. */
function subscriptionToJson(s: PushSubscription): string {
  const p256dh = s.getKey("p256dh");
  const auth = s.getKey("auth");
  if (p256dh === null || auth === null) {
    throw new Error("PushSubscription missing p256dh/auth keys");
  }
  return JSON.stringify({
    endpoint: s.endpoint,
    keys: {
      p256dh: bufferToBase64Url(p256dh),
      auth: bufferToBase64Url(auth),
    },
  });
}

function bufferToBase64Url(buf: ArrayBuffer): string {
  const bytes = new Uint8Array(buf);
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
}
