// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * This browser's Web Push token (ADR-0078 section 3).
 *
 * Before ADR-0078 the browser registered its token once at push-gw under a
 * random device pseudonym and subscribed that pseudonym to every group, so
 * push-gw held the devices x groups graph. Now push-gw holds no pseudonym and
 * no token in the clear: every group subscription carries the token sealed
 * under that group's unlock key (`group-routing.ts`
 * `subscribeGroupUnderPushGid`). So the token is kept here, on the device,
 * and sealed afresh into each subscription; when it changes, every
 * subscription of the signed-in account is sent again.
 *
 * `localStorage["lakko.push.token.v1"]` = base64 of the token bytes (the
 * JSON-serialised `PushSubscription`, `{endpoint, keys: {p256dh, auth}}`).
 * Per browser, not per account: one browser has one push subscription.
 * Removed by Log out and Forget device.
 */
import { base64Decode, base64Encode } from "../auth/account-id";

const STORAGE_KEY = "lakko.push.token.v1";

/** Longest token a subscription carries (mirror of `MAX_PUSH_TOKEN_LEN`). */
export const MAX_PUSH_TOKEN_LEN = 2048;

/** The stored token, or `null` when this browser has none (push off, not yet
 *  subscribed, signed out). */
export function getDevicePushToken(): Uint8Array | null {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw === null) return null;
    const bytes = base64Decode(raw);
    if (bytes.length === 0 || bytes.length > MAX_PUSH_TOKEN_LEN) {
      localStorage.removeItem(STORAGE_KEY);
      return null;
    }
    return bytes;
  } catch {
    return null;
  }
}

/** Store `token`. Returns whether it differs from the one stored before (the
 *  caller re-sends every subscription when it does). */
export function setDevicePushToken(token: Uint8Array): boolean {
  const before = getDevicePushToken();
  try {
    localStorage.setItem(STORAGE_KEY, base64Encode(token));
  } catch {
    // Quota / private mode: the caller still has the token in hand.
  }
  if (before === null || before.length !== token.length) return true;
  return before.some((b, i) => b !== token[i]);
}

/** Drop the stored token. */
export function clearDevicePushToken(): void {
  try {
    localStorage.removeItem(STORAGE_KEY);
  } catch {
    // Nothing to do.
  }
}
