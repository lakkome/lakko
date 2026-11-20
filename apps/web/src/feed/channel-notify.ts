// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Channel post notifications on web and the desktop app
 * (`docs/ux/channel-notifications.md`). The decision is the shared rule
 * (`lakko_ui_rules::channel_notify`, through wasm); this module only builds
 * its inputs from what the channel post sync read, and the notification's
 * words from what the Chats row shows. Pure: the wasm module is injected, so
 * `vitest` runs it against the real rule with no DOM.
 *
 * There is no push for channels yet: a post is noticed when this device
 * reads the channel (`ChannelPostSync.tsx`), while a Lakko tab or the
 * desktop window is open, hidden or not.
 */
import type { ChannelReadKind } from "../../wasm/lakko_ffi_wasm";
import type { Wasm } from "../wasm";
import type { ChannelPostSlot, PersistedChannelPost } from "./channel-post-log";

/** The kind of read, as the rule names it. */
export type ChannelReadKindName = "ongoing" | "first-after-subscribe" | "history";

/**
 * What kind of read produced a channel's new records. A channel with no
 * slot in the post log before this read is being read from its start: the
 * first read after subscribing when it has no read marker yet (the marker is
 * seeded right after that read, `feed-marks.ts` `seedChannelReads`), else a
 * refetch of history (the log was dropped by a format change, or the
 * channel's slot was lost while its marker survived). Joining as a publisher
 * keeps the channel's slot; a publisher's channel notifies nothing anyway.
 */
export function channelReadKind(
  slotBefore: ChannelPostSlot | undefined,
  readMarker: number | undefined,
): ChannelReadKindName {
  if (slotBefore !== undefined) return "ongoing";
  return readMarker === undefined ? "first-after-subscribe" : "history";
}

/** One channel's read, as the sync hands it over. */
export interface ChannelRead {
  readonly channelIdHex: string;
  /** The channel's slot before this read; `undefined` when it had none. */
  readonly slotBefore: ChannelPostSlot | undefined;
  /** The slot after it. */
  readonly slotAfter: ChannelPostSlot;
  /** The channel's read marker before this read; `undefined` when none. */
  readonly readMarker: number | undefined;
}

/** What the device knows of the channel when the read is judged. */
export interface ChannelNotifyFacts {
  readonly muted: boolean;
  /** The channel is open in front of the person (visible, focused). */
  readonly openAndVisible: boolean;
  /** This device publishes to the channel: every post counts as the
   *  person's own. A post signing key is random per device and channel, so
   *  a post from the person's other device cannot be told from a
   *  co-publisher's; the channel's row counts none of a publisher's channel
   *  as new either (`feed-marks.ts` `channelUnreadCount`). */
  readonly own: boolean;
  /** The cursor stored after this device's last read of the channel. */
  readonly lastNotified: number;
  readonly nowMs: number;
}

/** The decision for one read. */
export interface ChannelNotifyDecision {
  /** The post to show, the newest that notifies; `null` for none. */
  readonly post: PersistedChannelPost | null;
  /** The cursor to store for the channel. */
  readonly lastNotified: number;
}

type RuleWasm = Pick<Wasm, "ChannelNotifyContext" | "ChannelReadKind" | "channelPostsToNotify">;

function kindOf(wasm: RuleWasm, name: ChannelReadKindName): ChannelReadKind {
  switch (name) {
    case "ongoing":
      return wasm.ChannelReadKind.Ongoing;
    case "first-after-subscribe":
      return wasm.ChannelReadKind.FirstAfterSubscribe;
    case "history":
      return wasm.ChannelReadKind.History;
  }
}

/**
 * Judge one channel's read by the shared rule. The records are the posts
 * the read added (a post is a record that opened and is not a metadata
 * record, a ban list or an edit) and, when the read also passed over records
 * that are not posts, one non-post record at the slot's new cursor, so the
 * stored cursor moves past them too.
 */
export function decideChannelRead(
  wasm: RuleWasm,
  read: ChannelRead,
  facts: ChannelNotifyFacts,
): ChannelNotifyDecision {
  const before = read.slotBefore?.cursor ?? 0;
  const fresh = read.slotAfter.posts.filter((p) => p.cursor > before);
  const cursors = fresh.map((p) => p.cursor);
  const isPost = fresh.map(() => 1);
  const ts = fresh.map((p) => p.serverTsMs);
  if (!cursors.includes(read.slotAfter.cursor) && read.slotAfter.cursor > before) {
    cursors.push(read.slotAfter.cursor);
    isPost.push(0);
    ts.push(0);
  }
  const ctx = new wasm.ChannelNotifyContext(
    facts.muted,
    facts.openAndVisible,
    read.readMarker ?? 0,
    facts.lastNotified,
    kindOf(wasm, channelReadKind(read.slotBefore, read.readMarker)),
    facts.nowMs,
  );
  try {
    const own = new Uint8Array(cursors.length).fill(facts.own ? 1 : 0);
    const batch = wasm.channelPostsToNotify(ctx, Float64Array.from(cursors), Uint8Array.from(isPost), own, Float64Array.from(ts));
    try {
      const picked = Array.from(batch.notify);
      const last = picked.length === 0 ? undefined : picked[picked.length - 1];
      const post = last === undefined ? null : (fresh[last] ?? null);
      return { post, lastNotified: batch.lastNotified };
    } finally {
      batch.free();
    }
  } finally {
    ctx.free();
  }
}

/** Notifications of one channel replace each other. */
export function channelNotificationTag(channelIdHex: string): string {
  return `channel:${channelIdHex}`;
}
