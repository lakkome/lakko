// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import type { PersistedMessage } from "../chat/message-log-store";
import {
  buildFeedInput,
  FEED_MAX_WINDOW_MS,
  FEED_SEGMENT_ID_SPAN,
  feedRankId,
  feedRefLogKey,
  feedSourceKey,
  groupFeedSources,
  reactionSummary,
  type FeedSource,
} from "./feed-model";
import { withoutHidden } from "./feed-marks";
import { rankFeedInput } from "./feed-ranking";

const NOW = Date.parse("2026-09-29T12:00:00Z");
const HOUR = 3_600_000;

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

function msg(id: number, ageMs: number, extra: Partial<PersistedMessage> = {}): PersistedMessage {
  return {
    messageId: String(id),
    direction: "incoming",
    text: `message ${id}`,
    attachments: [],
    createdAt: new Date(NOW - ageMs).toISOString(),
    authorKeyB64: "author",
    ...extra,
  };
}

const GROUP: FeedSource = {
  kind: "group",
  idHex: "aa".repeat(32),
  slug: "slug",
  title: "Book club",
  muted: false,
  audience: 5,
};

function ctx(messages: PersistedMessage[], overrides: Partial<Parameters<typeof buildFeedInput>[0]> = {}) {
  return {
    sources: [GROUP],
    log: { [GROUP.idHex]: { cursor: "0", messages } },
    channelPosts: {},
    ownChannels: new Set<string>(),
    ownUsername: "alice",
    contactKeysB64: new Set(["friend"]),
    mentions: (text: string, username: string) => wasm.mentionsUsername(text, username),
    nowMs: NOW,
    ...overrides,
  };
}

describe("buildFeedInput", () => {
  it("leaves out system rows, deletions, bad ids and rows older than the window", () => {
    const input = buildFeedInput(
      ctx([
        msg(1, HOUR),
        msg(2, HOUR, { direction: "system" }),
        msg(3, HOUR, { deleted: true }),
        msg(4, HOUR, { messageId: "not-a-number" }),
        msg(5, FEED_MAX_WINDOW_MS + HOUR),
        msg(6, HOUR, { createdAt: "garbage" }),
      ]),
    );
    expect(input.candidates.map((c) => c.messageId)).toEqual([1n]);
    expect(input.refs[0]?.message.messageId).toBe("1");
  });

  it("derives the signals from the row", () => {
    const input = buildFeedInput(
      ctx([
        msg(1, 2 * HOUR, { direction: "outgoing", authorKeyB64: "me" }),
        msg(2, HOUR, {
          authorKeyB64: "friend",
          text: "hey @alice",
          reply: { target: "1" },
          reactions: [
            { reactorKeyB64: "x", emoji: "a" },
            { reactorKeyB64: "x", emoji: "b" },
            { reactorKeyB64: "y", emoji: "a" },
          ],
          attachments: [{} as never],
        }),
      ]),
    );
    const [own, reply] = input.candidates;
    expect(own?.fromSelf).toBe(true);
    expect(own?.mentionsMe).toBe(false);
    expect(reply).toMatchObject({
      fromSelf: false,
      fromContact: true,
      mentionsMe: true,
      repliesToMe: true,
      replyTo: 1n,
      reactionsDistinct: 2,
      hasMedia: true,
    });
  });

  it("does not match an email-like @ as a mention", () => {
    const input = buildFeedInput(ctx([msg(1, HOUR, { text: "mail bob@alice.com" })]));
    expect(input.candidates[0]?.mentionsMe).toBe(false);
  });
});

describe("reactionSummary", () => {
  it("counts per emoji, most first, at most three", () => {
    const m = msg(1, 0, {
      reactions: ["a", "b", "b", "c", "c", "c", "d"].map((emoji, i) => ({
        reactorKeyB64: String(i),
        emoji,
      })),
    });
    expect(reactionSummary(m)).toEqual([
      { emoji: "c", count: 3 },
      { emoji: "b", count: 2 },
      { emoji: "a", count: 1 },
    ]);
  });
});

describe("rankFeedInput (through the core)", () => {
  it("Latest is newest first with replies folded under their parent", () => {
    const input = buildFeedInput(
      ctx([
        msg(10, 5 * HOUR),
        msg(11, 3 * HOUR),
        msg(12, 2 * HOUR, { reply: { target: "10" } }),
        msg(13, HOUR, { reply: { target: "10" } }),
      ]),
    );
    const rule = wasm.FeedRule.preset(wasm.FeedPreset.Latest);
    const items = rankFeedInput(wasm, rule, input, NOW);
    expect(items.map((i) => i.ref.message.messageId)).toEqual(["11", "10"]);
    expect(items[1]?.replies.map((r) => r.message.messageId)).toEqual(["12", "13"]);
  });

  it("For me keeps only mentions, replies to me and my reacted-to messages", () => {
    const input = buildFeedInput(
      ctx([
        msg(1, 3 * HOUR, { direction: "outgoing", reactions: [{ reactorKeyB64: "z", emoji: "a" }] }),
        msg(2, 2 * HOUR, { text: "ping @alice" }),
        msg(3, HOUR),
      ]),
    );
    const rule = wasm.FeedRule.preset(wasm.FeedPreset.ForMe);
    const ids = rankFeedInput(wasm, rule, input, NOW).map((i) => i.ref.message.messageId);
    expect(ids.sort()).toEqual(["1", "2"]);
  });

  it("a muted group is left out unless the rule takes muted ones", () => {
    const input = buildFeedInput(ctx([msg(1, HOUR)], { sources: [{ ...GROUP, muted: true }] }));
    const rule = wasm.FeedRule.preset(wasm.FeedPreset.Latest);
    expect(rankFeedInput(wasm, rule, input, NOW)).toEqual([]);
    rule.includeMuted = true;
    expect(rankFeedInput(wasm, rule, input, NOW)).toHaveLength(1);
  });
});

describe("channels", () => {
  const CHANNEL: FeedSource = {
    kind: "channel",
    idHex: "cc".repeat(16),
    slug: "cc".repeat(16),
    title: "News",
    muted: false,
    audience: 0,
  };

  function channelCtx(own: boolean) {
    return ctx([msg(1, 3 * HOUR)], {
      sources: [GROUP, CHANNEL],
      channelPosts: {
        [CHANNEL.idHex]: {
          cursor: 7,
          posts: [
            { cursor: 6, serverTsMs: NOW - FEED_MAX_WINDOW_MS - HOUR, text: "too old" },
            { cursor: 7, serverTsMs: NOW - HOUR, text: "hello @alice" },
          ],
        },
      },
      ownChannels: new Set(own ? [CHANNEL.idHex] : []),
    });
  }

  it("turns posts inside the window into channel candidates", () => {
    const input = buildFeedInput(channelCtx(false));
    const post = input.candidates.find((c) => c.conversation === 1);
    expect(input.candidates.filter((c) => c.conversation === 1)).toHaveLength(1);
    expect(post).toMatchObject({
      messageId: 7n,
      fromSelf: false,
      mentionsMe: true,
      reactionsDistinct: 0,
      replyTo: null,
    });
    expect(input.refs.find((r) => r.source.kind === "channel")?.message.text).toBe("hello @alice");
  });

  it("a post in a channel this device publishes to is the person's own", () => {
    const input = buildFeedInput(channelCtx(true));
    const post = input.candidates.find((c) => c.conversation === 1);
    expect(post?.fromSelf).toBe(true);
    expect(post?.mentionsMe).toBe(false);
  });

  it("Latest ranks a newer post above an older group message", () => {
    const input = buildFeedInput(channelCtx(false));
    const rule = wasm.FeedRule.preset(wasm.FeedPreset.Latest);
    const kinds = rankFeedInput(wasm, rule, input, NOW).map((i) => i.ref.source.kind);
    expect(kinds).toEqual(["channel", "group"]);
  });
});

describe("the feed's sources are conversations, not groups (ADR-0065 R.5)", () => {
  const conv = (conversationHex: string, headHex: string) => ({
    conversationHex,
    headHex,
    slug: `slug-${headHex}`,
    title: `title-${conversationHex}`,
    audience: 3,
  });

  it("a conversation that moved is one source: drawn from its current group, keyed on the conversation", () => {
    const muted = new Set(["root"]);
    const sources = groupFeedSources([conv("root", "successor"), conv("plain", "plain")], (c) => muted.has(c));
    expect(sources).toEqual([
      {
        kind: "group",
        idHex: "successor",
        conversationHex: "root",
        slug: "slug-successor",
        title: "title-root",
        muted: true,
        audience: 3,
      },
      { kind: "group", idHex: "plain", slug: "slug-plain", title: "title-plain", muted: false, audience: 3 },
    ]);
    expect(sources.map(feedSourceKey)).toEqual(["root", "plain"]);
  });

  it("never lists one conversation twice", () => {
    expect(groupFeedSources([conv("root", "a"), conv("root", "b")], () => false)).toHaveLength(1);
  });

  it("hidden before the move stays hidden after it", () => {
    const sources = groupFeedSources([conv("root", "successor"), conv("plain", "plain")], () => false);
    expect(withoutHidden(sources, ["root"]).map((s) => s.idHex)).toEqual(["plain"]);
    // The successor's own id hides nothing: it is not what a person hid.
    expect(withoutHidden(sources, ["successor"])).toHaveLength(2);
  });
});

describe("an older group of a moved conversation feeds the same source (ADR-0065 R.5 / R.7 case 3)", () => {
  const ROOT = "11".repeat(32);
  const MIDDLE = "22".repeat(32);
  const HEAD = "33".repeat(32);
  const MOVED: FeedSource = {
    kind: "group",
    idHex: HEAD,
    conversationHex: ROOT,
    olderSegmentHexes: [ROOT, MIDDLE],
    slug: "head-slug",
    title: "Book club",
    muted: false,
    audience: 5,
  };
  const moved = (log: Record<string, PersistedMessage[]>, source: FeedSource = MOVED) =>
    ctx([], {
      sources: [source],
      log: Object.fromEntries(Object.entries(log).map(([hex, messages]) => [hex, { cursor: "0", messages }])),
    });

  it("a row still arriving in an old group's journal is an item of the conversation", () => {
    const input = buildFeedInput(
      moved({
        [ROOT]: [msg(40, 2 * HOUR, { text: "still writing in the old group" })],
        [HEAD]: [msg(1, HOUR, { text: "first in the successor" })],
      }),
    );
    expect(input.refs.map((r) => r.message.text)).toEqual([
      "still writing in the old group",
      "first in the successor",
    ]);
    // Both are items of the ONE source.
    expect(input.candidates.map((c) => c.conversation)).toEqual([0, 0]);
    expect(input.refs.every((r) => r.source === MOVED)).toBe(true);
    // The old row says which group it lives in and how far behind the head.
    expect(input.refs[0]?.segment).toEqual({ idHex: ROOT, distanceFromHead: 2 });
    expect(input.refs[1]?.segment).toBeUndefined();
    expect(feedRefLogKey(input.refs[0]!)).toBe(ROOT);
    expect(feedRefLogKey(input.refs[1]!)).toBe(HEAD);
  });

  it("the head's ids are unchanged and no two groups' rows share a rank id", () => {
    const input = buildFeedInput(
      moved({
        [ROOT]: [msg(1, 4 * HOUR), msg(2, 3 * HOUR)],
        [MIDDLE]: [msg(1, 2 * HOUR)],
        [HEAD]: [msg(1, HOUR), msg(2, HOUR / 2)],
      }),
    );
    const ids = input.candidates.map((c) => c.messageId);
    expect(new Set(ids).size).toBe(ids.length);
    expect(ids.slice(-2)).toEqual([1n, 2n]);
    expect(ids[0]).toBe(1n + 2n * FEED_SEGMENT_ID_SPAN);
    expect(ids[2]).toBe(1n + FEED_SEGMENT_ID_SPAN);
    expect(feedRankId(7n, 0)).toBe(7n);
  });

  it("a reply is folded under its parent inside its own group, never under a same-numbered row of another", () => {
    const input = buildFeedInput(
      moved({
        [ROOT]: [msg(10, 5 * HOUR, { text: "old parent" }), msg(11, 4 * HOUR, { text: "old reply", reply: { target: "10" } })],
        [HEAD]: [msg(10, 2 * HOUR, { text: "new parent" }), msg(12, HOUR, { text: "new reply", reply: { target: "10" } })],
      }),
    );
    const items = rankFeedInput(wasm, wasm.FeedRule.preset(wasm.FeedPreset.Latest), input, NOW);
    expect(items.map((i) => i.ref.message.text)).toEqual(["new parent", "old parent"]);
    expect(items[0]?.replies.map((r) => r.message.text)).toEqual(["new reply"]);
    expect(items[1]?.replies.map((r) => r.message.text)).toEqual(["old reply"]);
  });

  it("'replies to me' is judged inside the row's own group", () => {
    const input = buildFeedInput(
      moved({
        // My message 5 lives in the OLD group; a reply to 5 in the head
        // answers somebody else's row there.
        [ROOT]: [msg(5, 5 * HOUR, { direction: "outgoing" }), msg(6, 4 * HOUR, { reply: { target: "5" } })],
        [HEAD]: [msg(5, 2 * HOUR), msg(7, HOUR, { reply: { target: "5" } })],
      }),
    );
    const byText = new Map(input.refs.map((r) => [`${feedRefLogKey(r)}:${r.message.messageId}`, r.repliesToMe]));
    expect(byText.get(`${ROOT}:6`)).toBe(true);
    expect(byText.get(`${HEAD}:7`)).toBe(false);
  });

  it("an old group this device holds no log for adds nothing, and the window still applies", () => {
    const input = buildFeedInput(
      moved({
        [ROOT]: [msg(1, FEED_MAX_WINDOW_MS + HOUR), msg(2, HOUR)],
        [HEAD]: [],
      }),
    );
    expect(input.refs.map((r) => r.message.messageId)).toEqual(["2"]);
  });

  it("the source list names the older groups a conversation holds, oldest first", () => {
    const sources = groupFeedSources(
      [
        {
          conversationHex: ROOT,
          headHex: HEAD,
          olderSegmentHexes: [ROOT, MIDDLE],
          slug: "head-slug",
          title: "Book club",
          audience: 5,
        },
        { conversationHex: "plain", headHex: "plain", olderSegmentHexes: [], slug: "s", title: "t", audience: 2 },
      ],
      () => false,
    );
    expect(sources[0]?.olderSegmentHexes).toEqual([ROOT, MIDDLE]);
    expect(sources[0]?.idHex).toBe(HEAD);
    expect(sources[1] !== undefined && "olderSegmentHexes" in sources[1]).toBe(false);
  });
});
