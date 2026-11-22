// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/message-actions.md` section 9 — a notification already on screen
 * for a message that was then deleted is withdrawn.
 *
 * The service worker records, in each message notification's `data`, the DS
 * id of the message the preview stands for (`previewMessageId`, decimal) next
 * to the wake (whose `groupHex` names the group). When the app applies a
 * deletion it closes every notification of that group whose
 * `previewMessageId` is one of the deleted ids. A notification that stands
 * for no single message (the generic "New message", a reaction) carries no id
 * and is left alone. Never throws.
 */

/** The registration surface used here, injectable for the tests. */
export type NotificationRegistration = Pick<ServiceWorkerRegistration, "getNotifications">;

async function defaultRegistration(): Promise<NotificationRegistration | null | undefined> {
  if (typeof navigator === "undefined" || !("serviceWorker" in navigator)) return null;
  return navigator.serviceWorker.getRegistration();
}

/** Whether a notification's `data` names one of `ids` in `groupHex`. */
export function notificationStandsForDeleted(
  data: unknown,
  groupHex: string,
  ids: ReadonlySet<string>,
): boolean {
  if (typeof data !== "object" || data === null) return false;
  const d = data as { wake?: unknown; previewMessageId?: unknown };
  if (typeof d.previewMessageId !== "string" || !ids.has(d.previewMessageId)) return false;
  const wake = d.wake as { groupHex?: unknown } | undefined;
  return typeof wake?.groupHex === "string" && wake.groupHex.toLowerCase() === groupHex.toLowerCase();
}

/**
 * Close the notifications on screen for the deleted messages `messageIds` of
 * the group `groupHex`. Returns how many were closed.
 */
export async function withdrawDeletedMessageNotifications(
  groupHex: string,
  messageIds: readonly number[],
  getRegistration: () => Promise<NotificationRegistration | null | undefined> = defaultRegistration,
): Promise<number> {
  if (messageIds.length === 0) return 0;
  try {
    const reg = await getRegistration();
    if (reg === null || reg === undefined) return 0;
    const ids = new Set(messageIds.map((id) => String(id)));
    let closed = 0;
    for (const n of await reg.getNotifications()) {
      if (notificationStandsForDeleted(n.data, groupHex, ids)) {
        n.close();
        closed += 1;
      }
    }
    return closed;
  } catch {
    return 0;
  }
}
