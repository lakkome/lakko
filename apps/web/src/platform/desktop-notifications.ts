// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * When the desktop app raises a system notification for a message or a call
 * (`docs/ux/desktop.md` sections 4, 4.1 and 4.3). Pure, so the rules are
 * tested without a window; `DesktopBridge.tsx` feeds it and hands the result
 * to the host.
 *
 * The desktop has no push service and no service worker (ADR-0085): the page
 * keeps receiving while the window is hidden, so a message is announced when
 * a conversation's unread count rises. The first snapshot after start only
 * records the counts: messages that arrived while Lakko was not running are
 * in the list, not in a burst of notifications.
 */
import type { HostNotification } from "./host";

/** One conversation, as the notification rules need it. */
export interface NotifyRow {
  /** Unique across kinds. */
  readonly key: string;
  readonly unread: number;
  readonly muted: boolean;
  readonly title: string;
  /** The last message, one line; empty when there is nothing to show. */
  readonly preview: string;
  /** Where a click opens it; `null` when it cannot be opened. */
  readonly href: string | null;
  /** A channel's row: its posts are announced by the channel rule
   *  (`feed/ChannelPostNotifier.tsx`, `docs/ux/channel-notifications.md`),
   *  which keeps history and the person's own posts quiet; a rise in its
   *  count announces nothing here. */
  readonly channel?: boolean;
}

/** What the window looks like when a change is seen. */
export interface WindowView {
  /** The window is shown and has the focus. */
  readonly focused: boolean;
  /** The path open in the window. */
  readonly path: string;
}

/** Tag of a conversation's notification: a newer one replaces it. */
export function conversationTag(key: string): string {
  return `conversation:${key}`;
}

/**
 * The notifications a change of unread counts raises, and the counts to
 * compare the next change with. `previous === null` is the first snapshot:
 * nothing is announced.
 */
export function messageNotifications(
  previous: ReadonlyMap<string, number> | null,
  rows: readonly NotifyRow[],
  view: WindowView,
  newMessageWord: string,
): { next: Map<string, number>; notify: HostNotification[] } {
  const next = new Map<string, number>();
  const notify: HostNotification[] = [];
  for (const row of rows) {
    next.set(row.key, row.unread);
    if (previous === null) continue;
    const before = previous.get(row.key) ?? 0;
    if (row.unread <= before) continue;
    if (row.muted || row.href === null || row.channel === true) continue;
    // The conversation is on screen in front of the person.
    if (view.focused && view.path === row.href) continue;
    notify.push({
      tag: conversationTag(row.key),
      title: row.title,
      body: row.preview.trim() === "" ? newMessageWord : row.preview,
      target: row.href,
    });
  }
  return { next, notify };
}

/** Tag of a ringing call's notification: taken back when the ring ends. */
export function callTag(callId: string): string {
  return `call:${callId}`;
}

/** What a ringing call asks of the desktop. */
export type CallAlert =
  | { readonly kind: "none" }
  /** The window is not in front: a system notification. */
  | { readonly kind: "notify"; readonly notification: HostNotification }
  /** The window is in front with its own incoming-call screen: bounce the
   *  Dock icon / flash the taskbar once, never take the focus. */
  | { readonly kind: "attention" };

/**
 * The alert for a 1x1 call ringing on this device. A muted conversation's
 * call is offered quietly inside the app and raises nothing
 * (`docs/ux/calls.md` section 2.1.1). One alert per call id: the caller
 * passes the id it already alerted for.
 */
export function callAlert(input: {
  readonly ringingCallId: string | null;
  readonly quiet: boolean;
  readonly alreadyAlerted: string | null;
  readonly focused: boolean;
  readonly callerTitle: string;
  readonly incomingWord: string;
  readonly acceptWord: string;
  readonly declineWord: string;
  /** The caller offered video: Accept answers in kind. */
  readonly video: boolean;
}): CallAlert {
  const id = input.ringingCallId;
  if (id === null || input.quiet || id === input.alreadyAlerted) return { kind: "none" };
  if (input.focused) return { kind: "attention" };
  return {
    kind: "notify",
    notification: {
      tag: callTag(id),
      title: input.callerTitle,
      body: input.incomingWord,
      // The call screen is what the window shows; opening it is enough.
      target: null,
      call: { accept: input.acceptWord, decline: input.declineWord, callId: id, video: input.video },
    },
  };
}
