// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  cardsFromItems,
  mixSuggestions,
  rankCards,
  reasonLine,
  verifyDecision,
  type CardContext,
  type DecodedListing,
  type DirectoryCard,
} from "./cards";

const ID_A = "aa".repeat(16);
const ID_B = "bb".repeat(16);
const ID_C = "cc".repeat(16);

const listings: Record<string, DecodedListing> = {
  a: { channelIdHex: ID_A, handle: "alpha", name: "Alpha", description: "A", topic: 3, language: "en" },
  b: { channelIdHex: ID_B, handle: "beta", name: "Beta", description: "B", topic: 5, language: "de" },
  // A listing that names another channel than the item it came in.
  x: { channelIdHex: ID_A, handle: "evil", name: "Evil", description: "", topic: 3, language: "en" },
};
const decode = (b64: string): DecodedListing | null => listings[b64] ?? null;

const ctx = (over: Partial<CardContext> = {}): CardContext => ({
  pickedTopics: new Set([3]),
  language: "en",
  notInterested: new Set(),
  hiddenTopics: new Set(),
  following: new Set(),
  gone: new Set(),
  ...over,
});

describe("cards from directory items", () => {
  it("drops an undecodable listing and one naming another channel", () => {
    const cards = cardsFromItems(
      [
        { channel_id: ID_A, listing_b64: "a", subscribers: "1k+" },
        { channel_id: ID_C, listing_b64: "x", subscribers: "<10" },
        { channel_id: ID_B, listing_b64: "junk", subscribers: "<10" },
        { channel_id: ID_B, listing_b64: "b", subscribers: "bogus" },
      ],
      "bucket",
      decode,
    );
    expect(cards.map((c) => c.channelIdHex)).toEqual([ID_A, ID_B]);
    expect(cards[0]?.band).toBe("1k+");
    expect(cards[1]?.band).toBeNull();
  });
});

describe("ranking on the device", () => {
  const base = cardsFromItems(
    [
      { channel_id: ID_B, listing_b64: "b", subscribers: "100k+" },
      { channel_id: ID_A, listing_b64: "a", subscribers: "<10" },
    ],
    "bucket",
    decode,
  );

  it("puts a picked topic in the person's language first", () => {
    expect(rankCards(base, ctx()).map((c) => c.channelIdHex)).toEqual([ID_A, ID_B]);
  });

  it("hides not-interested, followed, gone and hidden-topic channels", () => {
    expect(rankCards(base, ctx({ notInterested: new Set([ID_A]) })).map((c) => c.channelIdHex)).toEqual([ID_B]);
    expect(rankCards(base, ctx({ following: new Set([ID_B]) })).map((c) => c.channelIdHex)).toEqual([ID_A]);
    expect(rankCards(base, ctx({ gone: new Set([ID_A, ID_B]) }))).toEqual([]);
    expect(rankCards(base, ctx({ hiddenTopics: new Set([5]) })).map((c) => c.channelIdHex)).toEqual([ID_A]);
  });

  it("merges a channel seen twice, keeping the group followers", () => {
    const group: DirectoryCard = {
      channelIdHex: ID_B,
      handle: "beta",
      name: "Beta",
      description: null,
      topic: null,
      language: null,
      band: null,
      source: "group",
      serverRank: 1000,
      followers: { groupIdB64: "g", groupTitle: "Book club", count: 2, contactNames: [] },
    };
    const out = rankCards([...base, group], ctx());
    expect(out).toHaveLength(2);
    const beta = out.find((c) => c.channelIdHex === ID_B);
    expect(beta?.description).toBe("B");
    expect(beta?.followers?.count).toBe(2);
    expect(beta?.source).toBe("group");
  });

  it("is total: equal scores order by name then id", () => {
    const same = (id: string, name: string): DirectoryCard => ({
      channelIdHex: id,
      handle: "h_" + name.toLowerCase(),
      name,
      description: "",
      topic: 9,
      language: "fr",
      band: null,
      source: "search",
      serverRank: 200,
      followers: null,
    });
    const out = rankCards([same(ID_C, "Zed"), same(ID_B, "Ann"), same(ID_A, "Ann")], ctx());
    expect(out.map((c) => c.channelIdHex)).toEqual([ID_A, ID_B, ID_C]);
  });
});

describe("reason line", () => {
  const card = (followers: DirectoryCard["followers"], source: DirectoryCard["source"] = "group"): DirectoryCard => ({
    channelIdHex: ID_A,
    handle: "alpha",
    name: "Alpha",
    description: null,
    topic: 3,
    language: "en",
    band: null,
    source,
    serverRank: 0,
    followers,
  });
  const topicName = (id: number): string => (id === 3 ? "Technology" : "Other");

  it("names contacts only, and counts the rest", () => {
    expect(reasonLine(card({ groupIdB64: "g", groupTitle: "Book club", count: 3, contactNames: ["Anna"] }), topicName)).toBe(
      "Followed by Anna and 2 others in Book club",
    );
    expect(reasonLine(card({ groupIdB64: "g", groupTitle: "Book club", count: 3, contactNames: [] }), topicName)).toBe(
      "Followed by 3 people in Book club",
    );
    expect(reasonLine(card({ groupIdB64: "g", groupTitle: "Book club", count: 1, contactNames: ["Anna"] }), topicName)).toBe(
      "Followed by Anna in Book club",
    );
    expect(reasonLine(card(null, "bucket"), topicName)).toBe("Popular in Technology");
    expect(reasonLine(card(null, "search"), topicName)).toBe("Matches your search");
  });
});

describe("the check before subscribing", () => {
  it("subscribes only when the handle leads to the card's channel", () => {
    expect(verifyDecision(ID_A, ID_A.toUpperCase())).toBe("ok");
    expect(verifyDecision(ID_A, ID_B)).toBe("gone");
    expect(verifyDecision(ID_A, null)).toBe("gone");
  });
});

describe("suggestions mixed into a feed", () => {
  it("never mixes at the rate never", () => {
    expect(mixSuggestions(30, 5, "never").every((s) => s.kind === "item")).toBe(true);
  });

  it("places one card after every N items, never first or last", () => {
    const slots = mixSuggestions(17, 5, "sometimes");
    const cards = slots.flatMap((s, i) => (s.kind === "card" ? [i] : []));
    expect(cards).toEqual([8, 17]);
    expect(slots[0]?.kind).toBe("item");
    expect(slots[slots.length - 1]?.kind).toBe("item");
    expect(mixSuggestions(8, 5, "sometimes").some((s) => s.kind === "card")).toBe(false);
  });

  it("runs out of cards quietly", () => {
    expect(mixSuggestions(100, 1, "rarely").filter((s) => s.kind === "card")).toHaveLength(1);
  });
});
