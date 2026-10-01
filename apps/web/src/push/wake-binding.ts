// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0042, "A wake is shown only for the signed-in account" — the
 * foreground half.
 *
 * A device has at most one active account, and the service worker must
 * never show another account's push. It decides that from ONE row, the wake
 * binding (`lakko-push-routing`/`wake_binding`, see
 * `group-routing-store.ts`): the device push pseudonym this browser
 * registered and the account it registered it for. Every wake carries `d`,
 * the pseudonym that subscribed; the worker handles a wake only when `d`
 * equals the bound pseudonym, and only as the bound account
 * (`wakeBoundAccount` in `public/sw-push-decrypt.js`).
 *
 * This module owns the three moments the row changes:
 *
 *   - **registration** ({@link recordWakeBinding}) — written after
 *     `push-gw` accepted the pseudonym for the signed-in account;
 *   - **a different account signing in** ({@link pseudonymMustRotate}) —
 *     the old account's pseudonym is not reused, so its subscriptions
 *     (still live if it never logged out) fail the gate;
 *   - **Log out / Forget device** ({@link releaseWakeBinding}) — the row is
 *     cleared FIRST, then this app's delivered notifications are closed.
 */

import {
  clearWakeBinding,
  putWakeBinding,
  type WakeBinding,
} from "./group-routing-store";
import { peekDevicePushId } from "./device-id";

/** What {@link releaseWakeBinding} touches, injectable for the tests. */
export interface ReleaseDeps {
  clearBinding: () => Promise<void>;
  getRegistration: () => Promise<
    Pick<ServiceWorkerRegistration, "getNotifications"> | null | undefined
  >;
}

/** What {@link recordWakeBinding} touches, injectable for the tests. */
export interface RecordDeps {
  putBinding: (binding: WakeBinding) => Promise<void>;
  currentPseudonym: () => Uint8Array | null;
}

const defaultReleaseDeps: ReleaseDeps = {
  clearBinding: clearWakeBinding,
  getRegistration: async () => {
    if (typeof navigator === "undefined" || !("serviceWorker" in navigator)) {
      return null;
    }
    return navigator.serviceWorker.getRegistration();
  },
};

const defaultRecordDeps: RecordDeps = {
  putBinding: putWakeBinding,
  currentPseudonym: peekDevicePushId,
};

/**
 * Must this sign-in mint a fresh device pseudonym? Yes when a binding names
 * ANOTHER account: that account's pseudonym (and the group subscriptions
 * registered under it, if it never logged out) must not wake this sign-in.
 * No binding (first sign-in, or after Log out, which also drops the
 * pseudonym) and a binding for the same account keep the current one.
 */
export function pseudonymMustRotate(
  existing: WakeBinding | null,
  accountIdB64: string,
): boolean {
  return existing !== null && existing.accountId !== accountIdB64;
}

/**
 * Record `(pseudonym, account)` after `push-gw` accepted the registration.
 *
 * Skipped when the pseudonym on disk is no longer the one just registered —
 * a Log out that ran while the registration was in flight has already
 * dropped it, and writing the binding back would bind a signed-out account.
 * Never throws: a failed write leaves the worker showing nothing, which is
 * the safe side.
 *
 * @returns whether the binding was written.
 */
export async function recordWakeBinding(
  accountIdB64: string,
  devicePseudonym: Uint8Array,
  deps: RecordDeps = defaultRecordDeps,
): Promise<boolean> {
  const current = deps.currentPseudonym();
  if (current === null || !bytesEqual(current, devicePseudonym)) return false;
  try {
    await deps.putBinding({
      devicePseudonymHex: toHex(devicePseudonym),
      accountId: accountIdB64,
    });
    return true;
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("push wake binding not recorded", err);
    return false;
  }
}

/**
 * Log out / Forget device: clear the binding, then close every notification
 * this app has on screen. The binding goes first so a wake arriving while
 * the rest of the sign-out runs is already dropped by the worker. Never
 * throws.
 *
 * @returns how many notifications were closed.
 */
export async function releaseWakeBinding(
  deps: ReleaseDeps = defaultReleaseDeps,
): Promise<number> {
  try {
    await deps.clearBinding();
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("push wake binding not cleared", err);
  }
  return closeDeliveredNotifications(deps.getRegistration);
}

/**
 * Close every notification this origin's service worker is showing
 * (`registration.getNotifications()` then `close()`). Never throws.
 */
export async function closeDeliveredNotifications(
  getRegistration: ReleaseDeps["getRegistration"] =
    defaultReleaseDeps.getRegistration,
): Promise<number> {
  try {
    const reg = await getRegistration();
    if (reg === null || reg === undefined) return 0;
    const shown = await reg.getNotifications();
    for (const n of shown) n.close();
    return shown.length;
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("delivered notifications not cleared", err);
    return 0;
  }
}

function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

function toHex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}
