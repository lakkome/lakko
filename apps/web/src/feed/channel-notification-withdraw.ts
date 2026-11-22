// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/message-actions.md` section 9, channels — a notification already
 * on screen for a channel post that was then deleted is withdrawn, as a chat
 * notification is (`push/deleted-notifications.ts`).
 *
 * A channel has one notification at a time (its tag is the channel), standing
 * for the newest post this device announced. The notifier records that post's
 * cursor: in memory beside the notification it showed, and in the `data` of a
 * notification the service worker shows. When a read of the channel takes
 * that post out of the channel's record (a delete op removes it from the post
 * log, `channel-post-log.ts`), the notification is closed. Never throws.
 *
 * The in-memory record does not survive a reload of the page: a notification
 * the page itself showed (the Notification API, the desktop host) is then
 * left alone. One the service worker showed carries the cursor and is found
 * after a reload too.
 */

import { CHANNEL_POST_RETENTION_MS, type ChannelPostSlot } from "./channel-post-log";

/** The `data` key naming the post a service-worker notification stands for. */
export const CHANNEL_POST_CURSOR_KEY = "lakkoChannelPostCursor";

/**
 * The posts a read took out of a channel's record: in the slot before it,
 * not in the slot after, and not merely past the retention window. These are
 * the posts a delete op removed. Pure.
 */
export function postsGoneIn(
  read: { readonly slotBefore: ChannelPostSlot | undefined; readonly slotAfter: ChannelPostSlot },
  nowMs: number,
): ReadonlySet<number> {
  const gone = new Set<number>();
  const before = read.slotBefore?.posts ?? [];
  if (before.length === 0) return gone;
  const kept = new Set(read.slotAfter.posts.map((p) => p.cursor));
  const oldest = nowMs - CHANNEL_POST_RETENTION_MS;
  for (const p of before) {
    if (!kept.has(p.cursor) && p.serverTsMs >= oldest) gone.add(p.cursor);
  }
  return gone;
}

/** The notification this page showed for a channel, by its tag. */
interface Shown {
  readonly cursor: number;
  /** Closes a notification the page holds (the Notification API); absent
   *  for one the desktop host or the service worker shows. */
  readonly close?: () => void;
}

const shown = new Map<string, Shown>();

/** Record the post a channel's notification now stands for (it replaces the
 *  channel's previous one). */
export function recordChannelNotification(tag: string, cursor: number, close?: () => void): void {
  shown.set(tag, close === undefined ? { cursor } : { cursor, close });
}

/** Forget a channel's record: the notification was tapped or closed. */
export function forgetChannelNotification(tag: string, cursor: number): void {
  if (shown.get(tag)?.cursor === cursor) shown.delete(tag);
}

/** What the withdrawal reaches, injectable for the tests. */
export interface WithdrawDeps {
  readonly getRegistration: () => Promise<Pick<ServiceWorkerRegistration, "getNotifications"> | null | undefined>;
  /** Takes a desktop host notification back; a no-op outside the app. */
  readonly withdrawHost: (tag: string) => Promise<void>;
}

/**
 * Close the notification of the channel tagged `tag` when it stands for one
 * of the `gone` posts. Returns how many were closed.
 */
export async function withdrawGoneChannelNotification(
  tag: string,
  gone: ReadonlySet<number>,
  deps: WithdrawDeps,
): Promise<number> {
  if (gone.size === 0) return 0;
  let closed = 0;
  const record = shown.get(tag);
  if (record !== undefined && gone.has(record.cursor)) {
    shown.delete(tag);
    try {
      if (record.close !== undefined) record.close();
      else await deps.withdrawHost(tag);
      closed += 1;
    } catch {
      // best-effort
    }
  }
  try {
    const reg = await deps.getRegistration();
    if (reg !== null && reg !== undefined) {
      for (const n of await reg.getNotifications({ tag })) {
        const cursor = (n.data as Record<string, unknown> | null | undefined)?.[CHANNEL_POST_CURSOR_KEY];
        if (typeof cursor === "number" && gone.has(cursor)) {
          n.close();
          closed += 1;
        }
      }
    }
  } catch {
    // best-effort
  }
  return closed;
}

/** Test seam: forget every record. */
export function resetChannelNotificationRecords(): void {
  shown.clear();
}
