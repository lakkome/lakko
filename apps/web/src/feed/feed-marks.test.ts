// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  advanceChannelRead,
  badgeText,
  cardsWithoutHidden,
  channelUnreadCount,
  decodeFeedMarks,
  dividerIndex,
  emptyFeedMarks,
  encodeFeedMarks,
  hideConversation,
  MAX_LOOKED_KEYS,
  pendingNewCount,
  recordLooked,
  seedChannelReads,
  showConversation,
  withoutHidden,
} from "./feed-marks";
import { feedForwardPayload } from "./feed-forward";
import { DEFAULT_FEED_ICON, FEED_ICONS, feedIconName } from "./feed-icons";

const GROUP = "ab".repeat(32);
const CHANNEL = "cd".repeat(16);
const OTHER = "ef".repeat(16);

describe("codec", () => {
  it("round-trips", () => {
    let m = hideConversation(emptyFeedMarks(), GROUP);
    m = recordLooked(m, "latest", ["a", "b"]);
    m = advanceChannelRead(m, CHANNEL, 7);
    expect(decodeFeedMarks(JSON.parse(JSON.stringify(encodeFeedMarks(m))))).toEqual(m);
  });

  it("drops malformed entries alone and refuses an unknown version", () => {
    expect(decodeFeedMarks({ version: 2, hidden: [GROUP] })).toEqual(emptyFeedMarks());
    const m = decodeFeedMarks({
      version: 1,
      hidden: [GROUP, "NOT-HEX", 3, GROUP],
      looked: { latest: ["a", 4], "bad key!": ["x"] },
      channelRead: { [CHANNEL]: 5, [OTHER]: -1, short: 3 },
    });
    expect(m.hidden).toEqual([GROUP]);
    expect(m.looked).toEqual({ latest: ["a"] });
    expect(m.channelRead).toEqual({ [CHANNEL]: 5 });
  });
});

describe("hide from feeds", () => {
  it("hides and shows again, idempotently", () => {
    const m = hideConversation(hideConversation(emptyFeedMarks(), GROUP), GROUP);
    expect(m.hidden).toEqual([GROUP]);
    expect(showConversation(m, GROUP).hidden).toEqual([]);
    expect(showConversation(m, CHANNEL)).toBe(m);
  });

  it("filters sources before ranking and suggestions by channel", () => {
    const sources = [{ idHex: GROUP }, { idHex: CHANNEL }];
    expect(withoutHidden(sources, [GROUP])).toEqual([{ idHex: CHANNEL }]);
    expect(withoutHidden(sources, [])).toEqual(sources);
    const cards = [{ channelIdHex: CHANNEL }, { channelIdHex: OTHER }];
    expect(cardsWithoutHidden(cards, new Set([OTHER]))).toEqual([{ channelIdHex: CHANNEL }]);
  });
});

describe("New divider", () => {
  it("has none on the first visit", () => {
    expect(dividerIndex(["a", "b"], undefined)).toBeNull();
  });

  it("has none when nothing is new", () => {
    expect(dividerIndex(["b", "a"], ["a", "b", "c"])).toBeNull();
  });

  it("sits above the first item that was not there", () => {
    expect(dividerIndex(["a", "x", "b", "y"], ["a", "b"])).toBe(1);
    expect(dividerIndex(["x", "a"], ["a"])).toBe(0);
  });

  it("remembers a bounded number of keys", () => {
    const keys = Array.from({ length: MAX_LOOKED_KEYS + 5 }, (_, i) => `k${i}`);
    expect(recordLooked(emptyFeedMarks(), "latest", keys).looked.latest).toHaveLength(MAX_LOOKED_KEYS);
  });
});

describe("Show N new", () => {
  it("counts waiting items the drawn list lacks", () => {
    expect(pendingNewCount(["a", "b"], ["c", "a", "d", "b"])).toBe(2);
    expect(pendingNewCount(["a"], ["a"])).toBe(0);
    expect(pendingNewCount(["a"], null)).toBe(0);
  });
});

describe("channel read state", () => {
  it("moves forward only", () => {
    const m = advanceChannelRead(emptyFeedMarks(), CHANNEL, 10);
    expect(advanceChannelRead(m, CHANNEL, 4)).toBe(m);
    expect(advanceChannelRead(m, CHANNEL, 12).channelRead[CHANNEL]).toBe(12);
    expect(advanceChannelRead(m, CHANNEL, 0)).toBe(m);
  });

  it("counts posts newer than the newest read", () => {
    expect(channelUnreadCount([1, 2, 3, 4], 2, false)).toBe(2);
    expect(channelUnreadCount([1, 2, 3], 0, false)).toBe(3);
  });

  it("starts a fresh subscription with no new posts; later posts count", () => {
    // First sync: the slot appears with posts 1..3; no marker yet.
    expect(channelUnreadCount([1, 2, 3], undefined, false)).toBe(0);
    const seeded = seedChannelReads(emptyFeedMarks(), {
      [CHANNEL]: { cursor: 3, posts: [{ cursor: 1 }, { cursor: 2 }, { cursor: 3 }] },
      [OTHER]: { cursor: 0, posts: [] },
    });
    expect(seeded.channelRead).toEqual({ [CHANNEL]: 3, [OTHER]: 0 });
    expect(channelUnreadCount([1, 2, 3], seeded.channelRead[CHANNEL], false)).toBe(0);
    expect(channelUnreadCount([1, 2, 3, 4, 5], seeded.channelRead[CHANNEL], false)).toBe(2);
    expect(channelUnreadCount([1], seeded.channelRead[OTHER], false)).toBe(1);
  });

  it("never re-seeds a marked channel, and writes nothing when all are marked", () => {
    const m = advanceChannelRead(emptyFeedMarks(), CHANNEL, 2);
    const slots = { [CHANNEL]: { cursor: 9, posts: [{ cursor: 9 }] } };
    expect(seedChannelReads(m, slots)).toBe(m);
  });

  it("is zero once the newest is read, even with older ones unseen", () => {
    expect(channelUnreadCount([1, 2, 3], 3, false)).toBe(0);
  });

  it("counts none on a channel this device posts to", () => {
    expect(channelUnreadCount([1, 2, 3], undefined, true)).toBe(0);
  });

  it("draws the badge as the chat list does", () => {
    expect(badgeText(5)).toBe("5");
    expect(badgeText(100)).toBe("99+");
  });
});

describe("feed icons", () => {
  it("lists twelve in spec order", () => {
    expect(FEED_ICONS.map((i) => i.name)).toEqual([
      "feed",
      "star",
      "heart",
      "bolt",
      "book",
      "code",
      "music-note",
      "camera",
      "globe",
      "chat",
      "bell",
      "flag",
    ]);
  });

  it("draws the default for an index it does not know", () => {
    expect(feedIconName(3)).toBe("bolt");
    expect(feedIconName(12)).toBe(FEED_ICONS[DEFAULT_FEED_ICON]?.name);
    expect(feedIconName(255)).toBe("feed");
    expect(feedIconName(-1)).toBe("feed");
  });
});


describe("forward from the feed", () => {
  const base = {
    messageId: "42",
    direction: "incoming" as const,
    text: "hello",
    attachments: [],
    createdAt: "2026-09-30T10:00:00.000Z",
  };

  it("carries a group message's author and origin", () => {
    const p = feedForwardPayload({
      kind: "group",
      message: base,
      originGroupIdB64: "AAAA",
      authorLabel: "Anna",
    });
    expect(p.text).toBe("hello");
    expect(p.forward).toEqual({ author: "Anna", origin: { groupIdB64: "AAAA", messageId: 42 } });
  });

  it("keeps the attribution of a message that is itself a forward", () => {
    const p = feedForwardPayload({
      kind: "group",
      message: { ...base, forward: { author: "Boris" } },
      originGroupIdB64: "AAAA",
      authorLabel: "Anna",
    });
    expect(p.forward).toEqual({ author: "Boris" });
  });

  it("carries a channel post's name and no origin", () => {
    const p = feedForwardPayload({ kind: "channel", message: base, channelTitle: "News" });
    expect(p.forward).toEqual({ author: "News" });
    expect(p.attachments).toEqual([]);
  });
});
