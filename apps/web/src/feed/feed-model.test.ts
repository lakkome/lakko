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
