// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0042, "A wake is shown only for the signed-in account" — the
 * foreground half.
 *
 * A device has at most one active account, and the service worker must
 * never show another account's push. It decides that from ONE row, the wake
 * binding (`lakko-push-routing`/`wake_binding`, see
 * `group-routing-store.ts`): the account this browser registered for push.
 * Since ADR-0078 there is no device pseudonym; every wake carries `s`, the
 * subscription id it was delivered for, and the worker handles it only when
 * the bound account's route row for the wake's `push_gid` holds that id
 * (`wakeBoundAccount` in `public/sw-push-decrypt.js`).
 *
 * This module owns the three moments the row changes:
 *
 *   - **registration** ({@link recordWakeBinding}) — written after the
 *     token was taken for the signed-in account;
 *   - **a different account signing in** ({@link previousBoundAccount}) —
 *     the account named by the old row has its push-gw subscriptions
 *     removed, and a wake for one of them would miss the new account's
 *     route rows anyway;
 *   - **Log out / Forget device** ({@link releaseWakeBinding}) — the row is
 *     cleared FIRST, then this app's delivered notifications are closed.
 */

import {
  clearWakeBinding,
  putWakeBinding,
  type WakeBinding,
} from "./group-routing-store";

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
};

/**
 * The account an existing binding names when it is NOT `accountIdB64` — the
 * account whose push-gw subscriptions this sign-in removes — or `null` (no
 * binding, or the same account).
 */
export function previousBoundAccount(
  existing: WakeBinding | null,
  accountIdB64: string,
): string | null {
  return existing !== null && existing.accountId !== accountIdB64 ? existing.accountId : null;
}

/**
 * Record `accountIdB64` as the account registered for push on this browser.
 * Never throws: a failed write leaves the worker showing nothing, which is
 * the safe side.
 *
 * @returns whether the binding was written.
 */
export async function recordWakeBinding(
  accountIdB64: string,
  deps: RecordDeps = defaultRecordDeps,
): Promise<boolean> {
  try {
    await deps.putBinding({ accountId: accountIdB64 });
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
