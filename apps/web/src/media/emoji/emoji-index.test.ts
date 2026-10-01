// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 §3 / UX spec §4 — emoji search, the Recent row, skin-tone
 * defaults and the one-grapheme reaction check, against the real bundled
 * dataset.
 */
import { describe, expect, it } from "vitest";

import raw from "./emoji-data.json";
import { bindContactsAccount } from "../../contacts/local-store";
import {
  MAX_EMOJI_BYTES,
  MAX_RECENT,
  RECENT_KEY_PREFIX,
  RETIRED_EMOJI_KEYS,
  TONES_KEY_PREFIX,
  buildDataset,
  clearEmojiPrefsForAccount,
  displayEmoji,
  isSingleEmojiGrapheme,
  loadRecent,
  loadTonePrefs,
  pushRecent,
  saveRecent,
  saveTonePrefs,
  searchEmoji,
  withTonePref,
  type EmojiEntry,
  type KeyValueStore,
} from "./emoji-index";

const dataset = buildDataset(raw);

function memoryStore(): KeyValueStore & { data: Map<string, string> } {
  const data = new Map<string, string>();
  return {
    data,
    getItem: (k) => data.get(k) ?? null,
    setItem: (k, v) => {
      data.set(k, v);
    },
    removeItem: (k) => {
      data.delete(k);
    },
  };
}

const ACCOUNT_A = "QUFBQUFBQUFBQUFBQUFBQQ==";
const ACCOUNT_B = "QkJCQkJCQkJCQkJCQkJCQg==";

describe("dataset", () => {
  it("loads every group and resolves tone variants to their base", () => {
    expect(dataset.groups[0]).toBe("Smileys & Emotion");
    expect(dataset.groups).not.toContain("Component");
    expect(dataset.entries.length).toBeGreaterThan(1500);
    const wave = dataset.byEmoji.get("\u{1F44B}\u{1F3FD}");
    expect(wave?.name).toBe("waving hand");
  });

  it("drops malformed rows instead of failing", () => {
    const ds = buildDataset({ groups: ["A"], emoji: [["x", "ok", "", 0], ["y", 5, "", 0], ["z", "bad group", "", 9]] });
    expect(ds.entries.map((e) => e.name)).toEqual(["ok"]);
  });
});

describe("search", () => {
  it("matches names and keywords, best matches first", () => {
    const cats = searchEmoji(dataset, "cat");
    expect(cats[0]?.name).toBe("cat");
    expect(cats.map((e) => e.name)).toContain("cat face");
  });

  it("requires every token", () => {
    const hits = searchEmoji(dataset, "red heart");
    expect(hits[0]?.name).toBe("red heart");
    expect(hits.every((e) => /red/.test(e.name + e.keywords.join(" ")))).toBe(true);
  });

  it("finds by keyword alone", () => {
    expect(searchEmoji(dataset, "grin").map((e) => e.emoji)).toContain("\u{1F600}");
  });

  it("empty and nonsense queries find nothing", () => {
    expect(searchEmoji(dataset, "   ")).toEqual([]);
    expect(searchEmoji(dataset, "zzqxv")).toEqual([]);
  });
});

describe("recent", () => {
  it("moves a reuse to the front and caps at 24", () => {
    let list: string[] = [];
    for (let i = 0; i < 30; i += 1) list = pushRecent(list, dataset.entries[i]!.emoji);
    expect(list).toHaveLength(MAX_RECENT);
    const again = pushRecent(list, list[10]!);
    expect(again[0]).toBe(list[10]);
    expect(new Set(again).size).toBe(MAX_RECENT);
  });

  it("round-trips through storage and ignores anything that is not an emoji", () => {
    const store = memoryStore();
    saveRecent(["\u{1F600}", "\u{1F44D}"], store, ACCOUNT_A);
    expect(loadRecent(store, ACCOUNT_A)).toEqual(["\u{1F600}", "\u{1F44D}"]);
    store.data.set(RECENT_KEY_PREFIX + ACCOUNT_A, JSON.stringify(["ok?", "\u{1F600}", 5]));
    expect(loadRecent(store, ACCOUNT_A)).toEqual(["\u{1F600}"]);
    store.data.set(RECENT_KEY_PREFIX + ACCOUNT_A, "{not json");
    expect(loadRecent(store, ACCOUNT_A)).toEqual([]);
    expect(loadRecent(null, ACCOUNT_A)).toEqual([]);
  });
});

describe("recents and tones belong to the account", () => {
  const wave = (): EmojiEntry => dataset.byEmoji.get("\u{1F44B}")!;

  it("two accounts keep separate recents and tones", () => {
    const store = memoryStore();
    const w = wave();
    saveRecent(["\u{1F600}"], store, ACCOUNT_A);
    saveRecent(["\u{1F44D}"], store, ACCOUNT_B);
    saveTonePrefs(withTonePref({}, w, w.tones![0]!), store, ACCOUNT_A);
    saveTonePrefs(withTonePref({}, w, w.tones![4]!), store, ACCOUNT_B);
    expect(loadRecent(store, ACCOUNT_A)).toEqual(["\u{1F600}"]);
    expect(loadRecent(store, ACCOUNT_B)).toEqual(["\u{1F44D}"]);
    expect(displayEmoji(w, loadTonePrefs(store, ACCOUNT_A))).toBe(w.tones![0]);
    expect(displayEmoji(w, loadTonePrefs(store, ACCOUNT_B))).toBe(w.tones![4]);
  });

  it("the default owner is the signed-in account", () => {
    const store = memoryStore();
    try {
      bindContactsAccount(ACCOUNT_A);
      saveRecent(["\u{1F600}"], store);
      bindContactsAccount(ACCOUNT_B);
      expect(loadRecent(store)).toEqual([]);
      saveRecent(["\u{1F44D}"], store);
      bindContactsAccount(ACCOUNT_A);
      expect(loadRecent(store)).toEqual(["\u{1F600}"]);
    } finally {
      bindContactsAccount(null);
    }
  });

  it("with no account a read is empty and a write is a no-op", () => {
    const store = memoryStore();
    bindContactsAccount(null);
    // A row left under the retired device-wide key is never read.
    store.data.set(RETIRED_EMOJI_KEYS[0], JSON.stringify(["\u{1F600}"]));
    store.data.set(RETIRED_EMOJI_KEYS[1], JSON.stringify({ "\u{1F44B}": wave().tones![1] }));
    expect(loadRecent(store)).toEqual([]);
    expect(loadTonePrefs(store)).toEqual({});
    const before = new Map(store.data);
    saveRecent(["\u{1F44D}"], store);
    saveTonePrefs({ "\u{1F44B}": wave().tones![2]! }, store);
    expect(store.data).toEqual(before);
  });

  it("the wipe removes only the wiped account's rows and the retired rows", () => {
    const store = memoryStore();
    saveRecent(["\u{1F600}"], store, ACCOUNT_A);
    saveRecent(["\u{1F44D}"], store, ACCOUNT_B);
    saveTonePrefs({ "\u{1F44B}": wave().tones![0]! }, store, ACCOUNT_A);
    saveTonePrefs({ "\u{1F44B}": wave().tones![4]! }, store, ACCOUNT_B);
    for (const key of RETIRED_EMOJI_KEYS) store.data.set(key, "[]");
    clearEmojiPrefsForAccount(ACCOUNT_A, store);
    expect(store.data.has(RECENT_KEY_PREFIX + ACCOUNT_A)).toBe(false);
    expect(store.data.has(TONES_KEY_PREFIX + ACCOUNT_A)).toBe(false);
    for (const key of RETIRED_EMOJI_KEYS) expect(store.data.has(key)).toBe(false);
    expect(loadRecent(store, ACCOUNT_B)).toEqual(["\u{1F44D}"]);
    expect(displayEmoji(wave(), loadTonePrefs(store, ACCOUNT_B))).toBe(wave().tones![4]);
  });
});

describe("skin tones", () => {
  it("a chosen tone becomes the default and choosing the base clears it", () => {
    const wave = dataset.byEmoji.get("\u{1F44B}")!;
    const dark = wave.tones![4]!;
    let prefs = withTonePref({}, wave, dark);
    expect(displayEmoji(wave, prefs)).toBe(dark);
    const store = memoryStore();
    saveTonePrefs(prefs, store, ACCOUNT_A);
    expect(displayEmoji(wave, loadTonePrefs(store, ACCOUNT_A))).toBe(dark);
    prefs = withTonePref(prefs, wave, wave.emoji);
    expect(displayEmoji(wave, prefs)).toBe(wave.emoji);
  });

  it("a stored variant that is not one of the entry's tones is ignored", () => {
    const wave = dataset.byEmoji.get("\u{1F44B}")!;
    expect(displayEmoji(wave, { [wave.emoji]: "\u{1F600}" })).toBe(wave.emoji);
  });
});

describe("one emoji grapheme", () => {
  it("accepts single emoji of every shape the picker produces", () => {
    for (const e of [
      "\u{1F600}",
      "❤️",
      "\u{1F44B}\u{1F3FF}",
      "\u{1F468}‍\u{1F469}‍\u{1F467}",
      "\u{1F1FA}\u{1F1F8}",
      "#️⃣",
      "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}",
    ]) {
      expect(isSingleEmojiGrapheme(e), e).toBe(true);
    }
  });

  it("every emoji in the bundled dataset passes", () => {
    const failing = dataset.entries.filter((e) => !isSingleEmojiGrapheme(e.emoji)).map((e) => e.name);
    expect(failing).toEqual([]);
  });

  it("refuses text, two emoji, trailing text and empty", () => {
    for (const bad of ["", "a", "ab", "\u{1F600}\u{1F600}", "\u{1F600}a", "1", "\u{1F1FA}"]) {
      expect(isSingleEmojiGrapheme(bad), JSON.stringify(bad)).toBe(false);
    }
  });

  it("refuses anything over the wire bound", () => {
    const long = "\u{1F468}‍".repeat(20) + "\u{1F468}";
    expect(new TextEncoder().encode(long).length).toBeGreaterThan(MAX_EMOJI_BYTES);
    expect(isSingleEmojiGrapheme(long)).toBe(false);
  });
});
