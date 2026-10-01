// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 §3 / UX spec §4 — the emoji picker's data model, search, the
 * Recent row and skin-tone defaults, and the one-grapheme check a reaction
 * must pass before it is sent.
 *
 * The dataset is `emoji-data.json`, generated offline from pinned Unicode
 * sources by `scripts/gen-emoji-data.mjs` and bundled with the app: nothing
 * here touches the network.
 */
import { boundContactsAccount } from "../../contacts/local-store";

/** Row shape written by the generator: `[emoji, name, "kw|kw", group]`,
 *  optionally followed by the five single-tone variants. */
type RawRow = readonly [string, string, string, number, (readonly string[])?];

export interface RawEmojiData {
  readonly groups: readonly string[];
  readonly emoji: readonly (readonly unknown[])[];
}

export interface EmojiEntry {
  readonly emoji: string;
  readonly name: string;
  readonly keywords: readonly string[];
  readonly group: number;
  /** light, medium-light, medium, medium-dark, dark — present only when all
   *  five exist. */
  readonly tones: readonly string[] | undefined;
}

export interface EmojiDataset {
  readonly groups: readonly string[];
  readonly entries: readonly EmojiEntry[];
  /** Every base emoji AND every tone variant, mapped to its base entry. */
  readonly byEmoji: ReadonlyMap<string, EmojiEntry>;
}

/** Build the dataset, skipping any row that does not have the expected shape
 *  rather than failing the picker. */
export function buildDataset(raw: RawEmojiData): EmojiDataset {
  const groups = raw.groups.filter((g): g is string => typeof g === "string");
  const entries: EmojiEntry[] = [];
  const byEmoji = new Map<string, EmojiEntry>();
  for (const row of raw.emoji) {
    const [emoji, name, keywords, group, tones] = row as unknown as RawRow;
    if (
      typeof emoji !== "string" ||
      typeof name !== "string" ||
      typeof keywords !== "string" ||
      typeof group !== "number" ||
      group < 0 ||
      group >= groups.length
    ) {
      continue;
    }
    const toneList =
      Array.isArray(tones) && tones.length === 5 && tones.every((t) => typeof t === "string")
        ? tones
        : undefined;
    const entry: EmojiEntry = {
      emoji,
      name,
      keywords: keywords.length === 0 ? [] : keywords.split("|"),
      group,
      tones: toneList,
    };
    entries.push(entry);
    byEmoji.set(emoji, entry);
    for (const t of toneList ?? []) byEmoji.set(t, entry);
  }
  return { groups, entries, byEmoji };
}

function words(text: string): string[] {
  return text
    .toLowerCase()
    .split(/[\s:,.()\-_'’]+/u)
    .filter((w) => w.length > 0);
}

/**
 * Entries matching `query`, best first. Every query token must be a prefix of
 * a word in the name or of a keyword. Ranking: exact name, then a name that
 * starts with the query, then a name-word match, then keyword-only matches;
 * ties keep the dataset's CLDR order. An empty query matches nothing (the
 * picker shows categories instead).
 */
export function searchEmoji(
  dataset: EmojiDataset,
  query: string,
  limit: number = 200,
): EmojiEntry[] {
  const q = query.trim().toLowerCase();
  if (q.length === 0) return [];
  const tokens = words(q);
  if (tokens.length === 0) return [];
  const scored: { entry: EmojiEntry; score: number; order: number }[] = [];
  dataset.entries.forEach((entry, order) => {
    const name = entry.name.toLowerCase();
    const nameWords = words(name);
    const keywordWords = entry.keywords.flatMap(words);
    let allInName = true;
    for (const t of tokens) {
      const inName = nameWords.some((w) => w.startsWith(t));
      if (!inName) allInName = false;
      if (!inName && !keywordWords.some((w) => w.startsWith(t))) return;
    }
    let score = 3;
    if (name === q) score = 0;
    else if (name.startsWith(q)) score = 1;
    else if (allInName) score = 2;
    scored.push({ entry, score, order });
  });
  scored.sort((a, b) => a.score - b.score || a.order - b.order);
  return scored.slice(0, Math.max(0, limit)).map((s) => s.entry);
}

// ----- per-account preferences -------------------------------------------
//
// The Recent row and the skin-tone choices are the signed-in account's data
// (2026-10-01), stored in `localStorage` under the account: the trailing
// `owner` argument, otherwise the account bound through `bindContactsAccount`
// (`contacts/local-store.ts`). With no account a read is empty and a write is
// a no-op. Log out keeps them for the account's next sign-in; Forget device
// and Delete account drop them ({@link clearEmojiPrefsForAccount}). They stay
// on this browser only: nothing syncs them to the account's other devices.

/** §4 — the Recent row holds up to this many. */
export const MAX_RECENT = 24;
/** `<prefix><accountIdB64>` holds one account's Recent row. */
export const RECENT_KEY_PREFIX = "lakko.emoji.recent:";
/** `<prefix><accountIdB64>` holds one account's tone choices. */
export const TONES_KEY_PREFIX = "lakko.emoji.tones:";
/** The retired device-wide rows. Never read; dropped by Forget device. */
export const RETIRED_EMOJI_KEYS = ["lakko.emoji.recent", "lakko.emoji.tones"] as const;

export type KeyValueStore = Pick<Storage, "getItem" | "setItem" | "removeItem">;

function defaultStore(): KeyValueStore | null {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    return null;
  }
}

function accountOf(owner: string | undefined): string | null {
  const account = owner ?? boundContactsAccount();
  return account === null || account.length === 0 ? null : account;
}

/** Move `emoji` to the front, de-duplicated, capped at `max`. */
export function pushRecent(
  list: readonly string[],
  emoji: string,
  max: number = MAX_RECENT,
): string[] {
  return [emoji, ...list.filter((e) => e !== emoji)].slice(0, Math.max(0, max));
}

/** The Recent row of `owner` (default: the signed-in account); empty with no
 *  account. */
export function loadRecent(
  store: KeyValueStore | null = defaultStore(),
  owner?: string,
): string[] {
  const account = accountOf(owner);
  if (store === null || account === null) return [];
  try {
    const parsed: unknown = JSON.parse(store.getItem(RECENT_KEY_PREFIX + account) ?? "[]");
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter((e): e is string => typeof e === "string" && isSingleEmojiGrapheme(e))
      .slice(0, MAX_RECENT);
  } catch {
    return [];
  }
}

/** Store the Recent row of `owner` (default: the signed-in account); a no-op
 *  with no account. */
export function saveRecent(
  list: readonly string[],
  store: KeyValueStore | null = defaultStore(),
  owner?: string,
): void {
  const account = accountOf(owner);
  if (store === null || account === null) return;
  try {
    store.setItem(RECENT_KEY_PREFIX + account, JSON.stringify(list.slice(0, MAX_RECENT)));
  } catch {
    // Storage full or blocked: the Recent row is a convenience.
  }
}

/** Base emoji → the tone variant chosen for it by this account. */
export type TonePrefs = Readonly<Record<string, string>>;

/** The tone choices of `owner` (default: the signed-in account); empty with
 *  no account. */
export function loadTonePrefs(
  store: KeyValueStore | null = defaultStore(),
  owner?: string,
): TonePrefs {
  const account = accountOf(owner);
  if (store === null || account === null) return {};
  try {
    const parsed: unknown = JSON.parse(store.getItem(TONES_KEY_PREFIX + account) ?? "{}");
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    const out: Record<string, string> = {};
    for (const [k, v] of Object.entries(parsed as Record<string, unknown>)) {
      if (typeof v === "string") out[k] = v;
    }
    return out;
  } catch {
    return {};
  }
}

/** Record `variant` as the default for `entry`. Choosing the base emoji
 *  itself clears the preference. Returns the new map. */
export function withTonePref(prefs: TonePrefs, entry: EmojiEntry, variant: string): TonePrefs {
  const next: Record<string, string> = { ...prefs };
  if (variant === entry.emoji || !(entry.tones ?? []).includes(variant)) {
    delete next[entry.emoji];
  } else {
    next[entry.emoji] = variant;
  }
  return next;
}

/** Store the tone choices of `owner` (default: the signed-in account); a
 *  no-op with no account. */
export function saveTonePrefs(
  prefs: TonePrefs,
  store: KeyValueStore | null = defaultStore(),
  owner?: string,
): void {
  const account = accountOf(owner);
  if (store === null || account === null) return;
  try {
    store.setItem(TONES_KEY_PREFIX + account, JSON.stringify(prefs));
  } catch {
    // A convenience, as above.
  }
}

/** Drop `accountIdB64`'s Recent row and tone choices, and the retired
 *  device-wide rows (Forget device, Delete account). Another account's rows
 *  stay. */
export function clearEmojiPrefsForAccount(
  accountIdB64: string,
  store: KeyValueStore | null = defaultStore(),
): void {
  if (store === null || accountIdB64.length === 0) return;
  store.removeItem(RECENT_KEY_PREFIX + accountIdB64);
  store.removeItem(TONES_KEY_PREFIX + accountIdB64);
  for (const key of RETIRED_EMOJI_KEYS) store.removeItem(key);
}

/** What the grid shows (and inserts) for `entry` under `prefs`. */
export function displayEmoji(entry: EmojiEntry, prefs: TonePrefs): string {
  const pref = prefs[entry.emoji];
  return pref !== undefined && (entry.tones ?? []).includes(pref) ? pref : entry.emoji;
}

// ----- reaction validation ------------------------------------------------

/** `lakko-attachments::MAX_EMOJI_BYTES` — the wire bound on a reaction. */
export const MAX_EMOJI_BYTES = 64;

const EXTENDED_PICTOGRAPHIC = /\p{Extended_Pictographic}/u;
const FLAG = /^\p{Regional_Indicator}{2}$/u;
const KEYCAP = /^[#*0-9]️?⃣$/u;
const LETTERS_OR_DIGITS = /[\p{L}\p{N}]/u;

/**
 * UX spec §4 — "A reaction is exactly one emoji grapheme". True when `text`
 * is one extended grapheme cluster that is an emoji (a pictographic sequence,
 * a regional-indicator flag or a keycap) and fits the wire bound. A letter,
 * two emoji, an emoji with trailing text, or an empty string are all refused.
 */
export function isSingleEmojiGrapheme(text: string): boolean {
  if (text.length === 0) return false;
  if (new TextEncoder().encode(text).length > MAX_EMOJI_BYTES) return false;
  const Segmenter = (Intl as { Segmenter?: typeof Intl.Segmenter }).Segmenter;
  if (Segmenter === undefined) return false;
  const segments = [...new Segmenter(undefined, { granularity: "grapheme" }).segment(text)];
  if (segments.length !== 1) return false;
  if (FLAG.test(text) || KEYCAP.test(text)) return true;
  if (!EXTENDED_PICTOGRAPHIC.test(text)) return false;
  // A pictograph glued to letters or digits in one cluster (possible with a
  // combining sequence) is not an emoji the picker could produce.
  return !LETTERS_OR_DIGITS.test(text.replace(/\p{Extended_Pictographic}/gu, ""));
}
