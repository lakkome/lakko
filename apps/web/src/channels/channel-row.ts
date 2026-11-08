// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The row model for the channel list pane (docs/ux/client-redesign.md
 * section 5.4): the same vocabulary as a chat row. Pure, so the rules for
 * the title, the handle line and the time label can be asserted without a
 * component.
 */

import { formatListTime } from "../chat/list-time";
import { joinFacts, t } from "../l10n";

/** What the row model needs to know about one channel. */
export interface ChannelRowInput {
  readonly id: string;
  /** ADR-0051 — the name from the newest shared metadata record, when one
   *  has been read; empty / absent otherwise. Wins over the local name. */
  readonly sharedName?: string | undefined;
  /** The locally stored name; empty when the user never set one. */
  readonly localName: string;
  /** The `@handle` the channel was created or found under, if any. */
  readonly localHandle?: string | undefined;
  /** This account's role in the channel (ADR-0087 section 15): the owner,
   *  a publisher, or a subscriber. */
  readonly role: "owner" | "publisher" | "subscriber";
  /** The subscriber count, when the device has fetched it. */
  readonly subscriberCount: number | null;
  /** The server stamp of the newest cached post, if any. */
  readonly lastPostMs: number | null;
  /** ADR-0073 phase 2 (UX 16.6): this device owned the channel under
   *  account keys that were since reset, so it can no longer post to it.
   *  The second line is the reset's own sentence. Absent = `false`. */
  readonly orphanedByKeyReset?: boolean | undefined;
}

/** What the row renders. */
export interface ChannelRowModel {
  readonly title: string;
  /** "@handle", or "" when the channel has none. */
  readonly handle: string;
  /** The second line: the handle, the subscriber count, or the role. */
  readonly subtitle: string;
  /** The last post's time in the chat-list form, or "" when unknown. */
  readonly time: string;
}

/** Canonical "@handle" form: one leading at-sign, trimmed, or "". */
export function displayHandle(raw: string | undefined): string {
  const h = (raw ?? "").trim().replace(/^@+/, "");
  return h.length === 0 ? "" : `@${h}`;
}

/** "1 subscriber" / "12 subscribers". */
export function subscriberCountLabel(n: number): string {
  return t("channels.info.subscribers", { count: n });
}

/** Build the row. `now` is the clock the time label is relative to. */
export function channelRowModel(input: ChannelRowInput, now: Date = new Date()): ChannelRowModel {
  const handle = displayHandle(input.localHandle);
  const title = channelTitle(input.sharedName, input.localName, input.localHandle);
  const time =
    input.lastPostMs === null ? "" : formatListTime(new Date(input.lastPostMs).toISOString(), now);
  // UX 16.6 — on the device that reset the keys, the row says so; it is the
  // only place that knows, and a handle or a count would read as "still
  // yours".
  if (input.orphanedByKeyReset === true) {
    return { title, handle, subtitle: t("channels.keys_reset.row_sub"), time };
  }
  const parts: string[] = [];
  if (handle.length > 0) parts.push(handle);
  if (input.subscriberCount !== null) parts.push(subscriberCountLabel(input.subscriberCount));
  if (parts.length === 0) {
    // UX 15: a publisher is not merely subscribed; the line names the role
    // the Publishers list names it by.
    parts.push(
      input.role === "owner"
        ? t("channels.row.own")
        : input.role === "publisher"
          ? t("channels.publishers.role_publisher")
          : t("channels.subscribe.status.subscribed"),
    );
  }
  return { title, handle, subtitle: joinFacts(parts), time };
}

/** ADR-0051 §5 — the channel title: the shared name, else the local name,
 *  else the bare handle, else "Channel". */
export function channelTitle(
  sharedName: string | undefined,
  localName: string,
  localHandle: string | undefined,
): string {
  const shared = (sharedName ?? "").trim();
  if (shared.length > 0) return shared;
  const local = localName.trim();
  if (local.length > 0) return local;
  const handle = displayHandle(localHandle);
  return handle.length > 0 ? handle.slice(1) : t("channels.fallback_name");
}

/** Case-insensitive match on the title and the handle. */
export function channelRowMatches(model: ChannelRowModel, query: string): boolean {
  const q = query.trim().toLowerCase().replace(/^@/, "");
  if (q.length === 0) return true;
  return (
    model.title.toLowerCase().includes(q) || model.handle.toLowerCase().replace(/^@/, "").includes(q)
  );
}

/** UX navigation 2.1 — a channel row's second line: the newest post this
 *  device can read, flattened to one line; else `fallback` (the @name or the
 *  status line). A channel post has no sender to name. */
export function channelRowPreview(lastPostText: string | null, fallback: string): string {
  const text = (lastPostText ?? "").replace(/\s+/g, " ").trim();
  return text.length > 0 ? text : fallback;
}
