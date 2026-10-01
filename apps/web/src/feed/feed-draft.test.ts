// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  blankDraft,
  bytesFromFragment,
  decodeDraft,
  draftProblem,
  draftSummary,
  encodeDraft,
  exportDraft,
  parseWords,
  presetDraft,
  shareFileName,
  shareLink,
  type FeedDraft,
} from "./feed-draft";
import { decodeFeedState, encodeFeedState, removeFeed, upsertFeed } from "./feed-store";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const GROUP = "aa".repeat(32);
const CHANNEL = "cc".repeat(16);

function full(): FeedDraft {
  return {
    ...blankDraft(wasm, "Reading"),
    icon: 4,
    channels: false,
    include: [GROUP],
    exclude: [CHANNEL],
    includeMuted: true,
    weights: { recency: 40, reactions: 70, replies: 10, fromContacts: 0, mentionsMe: 100, hasMedia: 5 },
    halfLife: "week",
    windowDays: 7,
    mediaOnly: true,
    hideOwn: true,
    minReactions: 2,
    hideWords: ["spoiler"],
    onlyWords: ["book"],
    suggestions: "rarely",
  };
}

describe("draft <-> rule", () => {
  it("round-trips every field through the canonical bytes", () => {
    const d = full();
    expect(decodeDraft(wasm, encodeDraft(wasm, d))).toEqual(d);
  });

  it("a preset starts from the core's own rule", () => {
    const d = presetDraft(wasm, wasm.FeedPreset.ForMe, "Mine");
    expect(d.forMe).toBe(true);
    expect(d.name).toBe("Mine");
  });
});

describe("draftProblem", () => {
  it("is null for a valid draft and a sentence for each refusal", () => {
    expect(draftProblem(wasm, full())).toBeNull();
    expect(draftProblem(wasm, { ...full(), name: "" })).toMatch(/name/);
    expect(draftProblem(wasm, { ...full(), groups: false, channels: false })).toMatch(
      /groups, channels or both/,
    );
    expect(draftProblem(wasm, { ...full(), windowDays: 31 })).toMatch(/1 and 30 days/);
  });
});

describe("parseWords", () => {
  it("normalizes one word per line and drops blanks and repeats", () => {
    expect(parseWords(wasm, "  Spoiler \n\nspoiler\nbook")).toEqual(["spoiler", "book"]);
  });
});

describe("sharing", () => {
  it("export strips the conversations and says so", () => {
    const shared = decodeDraft(wasm, exportDraft(wasm, full()));
    expect(shared.include).toEqual([]);
    expect(shared.exclude).toEqual([]);
    expect(shared.sourcesStripped).toBe(true);
    expect(draftSummary(shared)[0]).toBe("Sources: all groups");
  });

  it("the link carries the rule in the fragment only", () => {
    const bytes = exportDraft(wasm, full());
    const link = shareLink("https://lakko.me", bytes);
    const url = new URL(link);
    expect(url.pathname).toBe("/feed/import");
    expect(bytesFromFragment(url.hash)).toEqual(bytes);
    expect(bytesFromFragment("")).toBeNull();
    expect(bytesFromFragment("#not base64!")).toBeNull();
  });

  it("a file that is not a feed is refused with the core's code", () => {
    let code: unknown = null;
    try {
      decodeDraft(wasm, new Uint8Array([1, 2, 3]));
    } catch (err) {
      code = (err as { feedRuleError?: unknown }).feedRuleError;
    }
    expect(code).toBe("wrongDomain");
  });

  it("names the file after the feed", () => {
    expect(shareFileName({ ...full(), name: "Book club / news" })).toBe("Book-club-news.lakkofeed");
    expect(shareFileName({ ...full(), name: "///" })).toBe("feed.lakkofeed");
  });
});

describe("feed store codec", () => {
  it("round-trips and drops malformed entries alone", () => {
    const rule = new Uint8Array([1, 2, 3]);
    const state = upsertFeed({ feeds: [] }, { id: "abcdefghij", rule });
    const back = decodeFeedState(JSON.parse(JSON.stringify(encodeFeedState(state))));
    expect(back.feeds).toEqual([{ id: "abcdefghij", rule }]);
    expect(
      decodeFeedState({ version: 1, feeds: [{ id: "x", rule: "AQ" }, { id: "abcdefghij", rule: 5 }] })
        .feeds,
    ).toEqual([]);
    expect(decodeFeedState({ version: 9, feeds: [] }).feeds).toEqual([]);
    expect(removeFeed(state, "abcdefghij").feeds).toEqual([]);
  });
});
