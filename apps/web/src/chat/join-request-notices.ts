// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Telling the owner and admins that a request to join is waiting
 * (`docs/ux/group-and-channel-privacy.md` section 8.1, amended 2026-11-23).
 *
 * Three signals read the requests this device holds (the retained set
 * `group.pendingExternalJoins()` lists): a bar at the top of the
 * conversation, the chat-list row's unread count, and one notification per
 * new request. This module is the rule; the store
 * (`join-request-notices-store.ts`), the notifier
 * (`JoinRequestNotifier.tsx`) and the conversation view apply it.
 *
 * Per device, per account and group, two sets keyed by the joiner's
 * signature key (base64):
 *
 * - `announced`: requests already announced, by a notification or by the
 *   bar. A person who asks again (the knock read emits the request again as
 *   "already pending") is not announced twice.
 * - `unshown`: requests this device read while the conversation was not open
 *   in front of the person. Each adds one to the row's unread count; the set
 *   empties when the bar is drawn in the open conversation.
 *
 * Both are pruned to the requests still waiting whenever the pending set is
 * read.
 *
 * Pure: no storage, no wasm, no Solid.
 */

/** When the `Join requests` heading reads `64+`, so does the bar (the
 *  members panel's own threshold). */
export const JOIN_REQUESTS_FULL = 64;

/** The query value that asks the group's info to scroll to `Join requests`. */
export const JOIN_REQUESTS_SECTION = "join-requests";

/** The query parameter that carries {@link JOIN_REQUESTS_SECTION}. */
export const INFO_SECTION_PARAM = "section";

/** The two per-group sets. Arrays in insertion order, no duplicates. */
export interface JoinNoticeSets {
  readonly announced: readonly string[];
  readonly unshown: readonly string[];
}

export const EMPTY_JOIN_NOTICES: JoinNoticeSets = { announced: [], unshown: [] };

/** The role gate: the owner or an admin, while the membership lasts. The
 *  same gate that shows `Approve` and `Reject` in the members panel. */
export function managesJoinRequests(role: "owner" | "admin" | "member" | null, membershipEnded: boolean): boolean {
  return (role === "owner" || role === "admin") && !membershipEnded;
}

/** What the bar says, or `null` when there is no bar. */
export type JoinRequestBar =
  | { readonly key: "chat.join_requests.banner"; readonly count: number }
  | { readonly key: "chat.join_requests.banner_full" };

export function joinRequestBar(input: { readonly manages: boolean; readonly pendingCount: number }): JoinRequestBar | null {
  if (!input.manages || input.pendingCount <= 0) return null;
  if (input.pendingCount >= JOIN_REQUESTS_FULL) return { key: "chat.join_requests.banner_full" };
  return { key: "chat.join_requests.banner", count: input.pendingCount };
}

/** The moment a request arrives. */
export interface JoinRequestArrival {
  /** The group's conversation is open in front of the person (the app or
   *  tab in the foreground). */
  readonly openAndVisible: boolean;
  /** The group is muted: no notification, the row follows the muted rule. */
  readonly muted: boolean;
  /** The platform lets this page raise a notification. */
  readonly notificationsAllowed: boolean;
}

/** The outcome of one arriving request. */
export interface JoinRequestArrivalOutcome {
  readonly sets: JoinNoticeSets;
  /** Raise the notification for this request. */
  readonly notify: boolean;
}

/**
 * One request event from the knock read. Already announced: nothing. Else it
 * is announced now; open in front of the person, the bar is the signal;
 * otherwise it counts as not shown and, unless muted or not allowed, raises a
 * notification.
 */
export function onJoinRequestArrived(
  sets: JoinNoticeSets,
  joinerKeyB64: string,
  ctx: JoinRequestArrival,
): JoinRequestArrivalOutcome {
  if (sets.announced.includes(joinerKeyB64)) return { sets, notify: false };
  const announced = [...sets.announced, joinerKeyB64];
  if (ctx.openAndVisible) return { sets: { announced, unshown: sets.unshown }, notify: false };
  const unshown = sets.unshown.includes(joinerKeyB64) ? sets.unshown : [...sets.unshown, joinerKeyB64];
  return { sets: { announced, unshown }, notify: !ctx.muted && ctx.notificationsAllowed };
}

/** The bar was drawn in the open, visible conversation: every request it
 *  counts is announced, and none is unshown any more. */
export function onJoinRequestBarShown(sets: JoinNoticeSets, pendingKeysB64: readonly string[]): JoinNoticeSets {
  const announced = [...sets.announced];
  for (const k of pendingKeysB64) if (!announced.includes(k)) announced.push(k);
  return { announced, unshown: [] };
}

/** Keep only the requests still waiting. */
export function pruneJoinNotices(sets: JoinNoticeSets, pendingKeysB64: readonly string[]): JoinNoticeSets {
  const keep = new Set(pendingKeysB64);
  return {
    announced: sets.announced.filter((k) => keep.has(k)),
    unshown: sets.unshown.filter((k) => keep.has(k)),
  };
}

/**
 * What a read of the group leaves of its sets: everything dropped once this
 * device is no longer owner or admin, or its membership ended (a demoted
 * admin's row must not stay unread); otherwise pruned to the requests still
 * waiting. Never adds anything: only a knock read announces.
 */
export function joinNoticesAfterRead(
  sets: JoinNoticeSets,
  read: { readonly manages: boolean; readonly active: boolean; readonly pendingKeysB64: readonly string[] },
): JoinNoticeSets {
  if (!read.manages || !read.active) return EMPTY_JOIN_NOTICES;
  return pruneJoinNotices(sets, read.pendingKeysB64);
}

/** Whether two set pairs hold the same keys in the same order. */
export function sameJoinNotices(a: JoinNoticeSets, b: JoinNoticeSets): boolean {
  return (
    a.announced.length === b.announced.length &&
    a.unshown.length === b.unshown.length &&
    a.announced.every((k, i) => b.announced[i] === k) &&
    a.unshown.every((k, i) => b.unshown[i] === k)
  );
}

/** The row's unread count: unread messages plus requests not yet shown. */
export function rowUnreadWithRequests(messageUnread: number, unshownCount: number): number {
  return messageUnread + Math.max(0, unshownCount);
}

/** The notification body: the joiner's name when there is one. */
export type JoinRequestBody =
  | { readonly key: "notification.join_request.body"; readonly params: { readonly name: string; readonly group: string } }
  | { readonly key: "notification.join_request.body_unnamed"; readonly params: { readonly group: string } };

export function joinRequestBody(name: string | null, group: string): JoinRequestBody {
  const trimmed = name?.trim() ?? "";
  if (trimmed.length === 0) return { key: "notification.join_request.body_unnamed", params: { group } };
  return { key: "notification.join_request.body", params: { name: trimmed, group } };
}

/** One notification per group and request. */
export function joinRequestNotificationTag(groupIdB64Url: string, joinerKeyB64Url: string): string {
  return `join-request:${groupIdB64Url}.${joinerKeyB64Url}`;
}

/** The group's info at `Join requests`, from the conversation's path. */
export function joinRequestsPath(chatPath: string): string {
  return `${chatPath}?info=group&${INFO_SECTION_PARAM}=${JOIN_REQUESTS_SECTION}`;
}
