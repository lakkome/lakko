// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A feed rule as plain data, for the editor (`docs/ux/feed.md` section 4.1).
 *
 * The rule itself — its bounds, its codec, what "valid" means — is
 * `lakko-feed` in the core; this module only moves the fields between the
 * core's `FeedRule` class and a plain object the editor's signals can hold,
 * and turns the core's frozen refusal codes into sentences. It decides no
 * bound of its own: every check is the core's `validate`.
 *
 * Conversation ids are the 32-byte group ids and 16-byte channel ids as
 * lowercase hex, the same keys the feed's sources use.
 */

import { base64UrlDecodeNoPad, base64UrlEncodeNoPad } from "../auth/account-id";
import type { Wasm } from "../wasm";

/** The slice of the wasm module the editor uses. */
export type FeedDraftCore = Pick<
  Wasm,
  "FeedRule" | "FeedPreset" | "FeedHalfLife" | "FeedSuggestions" | "normalizeFeedWord" | "feedMaxWeight"
>;

type Rule = InstanceType<FeedDraftCore["FeedRule"]>;

export type HalfLifeKey = "hours" | "day" | "week";
export type SuggestionsKey = "never" | "rarely" | "sometimes";

export interface FeedWeightsDraft {
  readonly recency: number;
  readonly reactions: number;
  readonly replies: number;
  readonly fromContacts: number;
  readonly mentionsMe: number;
  readonly hasMedia: number;
}

export interface FeedDraft {
  readonly name: string;
  /** Index into the twelve-icon list (`feed-icons.ts`, UX spec 4.1). An
   *  index this client does not know draws the default, and is kept as it
   *  is so a re-save does not rewrite someone else's choice. */
  readonly icon: number;
  readonly groups: boolean;
  readonly channels: boolean;
  readonly include: readonly string[];
  readonly exclude: readonly string[];
  readonly sourcesStripped: boolean;
  readonly includeMuted: boolean;
  readonly weights: FeedWeightsDraft;
  readonly halfLife: HalfLifeKey;
  readonly windowDays: number;
  readonly mediaOnly: boolean;
  readonly hideOwn: boolean;
  readonly forMe: boolean;
  readonly minReactions: number;
  readonly hideWords: readonly string[];
  readonly onlyWords: readonly string[];
  readonly suggestions: SuggestionsKey;
}

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

function fromHex(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length >> 1);
  for (let i = 0; i < out.length; i += 1) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

const idsOf = (arr: Array<unknown>): string[] =>
  arr.filter((v): v is Uint8Array => v instanceof Uint8Array).map(toHex);

/** Read every field of a core rule. */
export function draftFromRule(core: FeedDraftCore, rule: Rule): FeedDraft {
  const halfLife: HalfLifeKey =
    rule.halfLife === core.FeedHalfLife.FewHours
      ? "hours"
      : rule.halfLife === core.FeedHalfLife.Week
        ? "week"
        : "day";
  const suggestions: SuggestionsKey =
    rule.suggestions === core.FeedSuggestions.Rarely
      ? "rarely"
      : rule.suggestions === core.FeedSuggestions.Sometimes
        ? "sometimes"
        : "never";
  return {
    name: rule.name,
    icon: rule.icon,
    groups: rule.groups,
    channels: rule.channels,
    include: idsOf(rule.include),
    exclude: idsOf(rule.exclude),
    sourcesStripped: rule.sourcesStripped,
    includeMuted: rule.includeMuted,
    weights: {
      recency: rule.weightRecency,
      reactions: rule.weightReactions,
      replies: rule.weightReplies,
      fromContacts: rule.weightFromContacts,
      mentionsMe: rule.weightMentionsMe,
      hasMedia: rule.weightHasMedia,
    },
    halfLife,
    windowDays: rule.windowDays,
    mediaOnly: rule.mediaOnly,
    hideOwn: rule.hideOwn,
    forMe: rule.forMe,
    minReactions: rule.minReactions,
    hideWords: [...rule.hideWords],
    onlyWords: [...rule.onlyWords],
    suggestions,
  };
}

/** Build a core rule from a draft. The caller frees it. Nothing is checked
 *  here; `validate` on the result (or {@link draftProblem}) says whether it
 *  can be saved. */
export function ruleFromDraft(core: FeedDraftCore, draft: FeedDraft): Rule {
  const rule = new core.FeedRule(draft.name);
  rule.icon = draft.icon;
  rule.groups = draft.groups;
  rule.channels = draft.channels;
  rule.setInclude(draft.include.map(fromHex));
  rule.setExclude(draft.exclude.map(fromHex));
  rule.sourcesStripped = draft.sourcesStripped;
  rule.includeMuted = draft.includeMuted;
  rule.weightRecency = draft.weights.recency;
  rule.weightReactions = draft.weights.reactions;
  rule.weightReplies = draft.weights.replies;
  rule.weightFromContacts = draft.weights.fromContacts;
  rule.weightMentionsMe = draft.weights.mentionsMe;
  rule.weightHasMedia = draft.weights.hasMedia;
  rule.halfLife = {
    hours: core.FeedHalfLife.FewHours,
    day: core.FeedHalfLife.Day,
    week: core.FeedHalfLife.Week,
  }[draft.halfLife];
  rule.windowDays = draft.windowDays;
  rule.mediaOnly = draft.mediaOnly;
  rule.hideOwn = draft.hideOwn;
  rule.forMe = draft.forMe;
  rule.minReactions = draft.minReactions;
  rule.hideWords = [...draft.hideWords];
  rule.onlyWords = [...draft.onlyWords];
  rule.suggestions = {
    never: core.FeedSuggestions.Never,
    rarely: core.FeedSuggestions.Rarely,
    sometimes: core.FeedSuggestions.Sometimes,
  }[draft.suggestions];
  return rule;
}

/** A blank feed: "Latest", named. */
export function blankDraft(core: FeedDraftCore, name = "My feed"): FeedDraft {
  const rule = new core.FeedRule(name);
  try {
    return draftFromRule(core, rule);
  } finally {
    rule.free();
  }
}

/** A preset as a starting point for a new feed. */
export function presetDraft(core: FeedDraftCore, preset: number, name: string): FeedDraft {
  const rule = core.FeedRule.preset(preset);
  try {
    return { ...draftFromRule(core, rule), name };
  } finally {
    rule.free();
  }
}

/** The frozen core code carried by a refusal, or `null`. */
export function refusalCode(err: unknown): string | null {
  const code = (err as { feedRuleError?: unknown } | null)?.feedRuleError;
  return typeof code === "string" ? code : null;
}

/** The editor's line for a refusal code. */
export function problemText(code: string | null): string {
  switch (code) {
    case "name":
      return "Give the feed a name of up to 64 characters.";
    case "noKinds":
      return "Choose groups, channels or both.";
    case "weightOutOfRange":
      return "A slider is out of range.";
    case "windowOutOfRange":
      return "Look back between 1 and 30 days.";
    case "conversations":
      return "Too many conversations are picked.";
    case "strippedWithSources":
      return "This shared feed can't also name conversations.";
    case "words":
      return "Check the word lists: one word per line, up to 32 words.";
    case "tooLarge":
      return "This feed is too large to save.";
    default:
      return "This feed can't be saved as it is.";
  }
}

/** Why the draft cannot be saved, in one sentence, or `null` when it can. */
export function draftProblem(core: FeedDraftCore, draft: FeedDraft): string | null {
  let rule: Rule | null = null;
  try {
    rule = ruleFromDraft(core, draft);
    rule.validate();
    return null;
  } catch (err) {
    return problemText(refusalCode(err));
  } finally {
    rule?.free();
  }
}

/** One word per line, each through the core's word rule; the ones it
 *  refuses and repeats are dropped. */
export function parseWords(core: FeedDraftCore, text: string): string[] {
  const out: string[] = [];
  for (const line of text.split("\n")) {
    const w = core.normalizeFeedWord(line);
    if (w !== undefined && !out.includes(w)) out.push(w);
  }
  return out;
}

// ----- Sharing (UX spec section 4.1) --------------------------------------

/** The canonical bytes of a draft. Throws the core's refusal. */
export function encodeDraft(core: FeedDraftCore, draft: FeedDraft): Uint8Array {
  const rule = ruleFromDraft(core, draft);
  try {
    return rule.encode();
  } finally {
    rule.free();
  }
}

/** The shareable bytes: the rule with its conversations stripped, since
 *  they mean nothing to anyone else and naming them would tell what the
 *  sharer is in. */
export function exportDraft(core: FeedDraftCore, draft: FeedDraft): Uint8Array {
  const rule = ruleFromDraft(core, draft);
  try {
    const exported = rule.export();
    try {
      return exported.encode();
    } finally {
      exported.free();
    }
  } finally {
    rule.free();
  }
}

/** Decode shared or stored bytes. Throws the core's refusal; nothing of a
 *  refused file is kept. */
export function decodeDraft(core: FeedDraftCore, bytes: Uint8Array): FeedDraft {
  const rule = core.FeedRule.decode(bytes);
  try {
    return draftFromRule(core, rule);
  } finally {
    rule.free();
  }
}

/** The link that carries a shared feed in its fragment, never its path, so
 *  no server sees the rule. */
export function shareLink(origin: string, bytes: Uint8Array): string {
  return `${origin}/feed/import#${base64UrlEncodeNoPad(bytes)}`;
}

/** The bytes a share link's fragment carries, or `null` when there is none. */
export function bytesFromFragment(hash: string): Uint8Array | null {
  const raw = hash.startsWith("#") ? hash.slice(1) : hash;
  if (raw.length === 0 || !/^[A-Za-z0-9_-]+$/.test(raw)) return null;
  try {
    return base64UrlDecodeNoPad(raw);
  } catch {
    return null;
  }
}

/** The file name a shared feed downloads as. */
export function shareFileName(draft: FeedDraft): string {
  const base = draft.name.replace(/[^\p{L}\p{N} _-]/gu, "").trim().replace(/\s+/g, "-");
  return `${base.length > 0 ? base : "feed"}.lakkofeed`;
}

/** The importer's preview of a shared feed's rules (UX spec section 4.1). */
export function draftSummary(draft: FeedDraft): string[] {
  const lines: string[] = [];
  const kinds =
    draft.groups && draft.channels
      ? "all groups and channels"
      : draft.groups
        ? "all groups"
        : "all channels";
  lines.push(`Sources: ${kinds}`);
  if (draft.includeMuted) lines.push("Includes muted conversations");
  const ranked = (
    [
      ["reactions", draft.weights.reactions],
      ["replies", draft.weights.replies],
      ["your contacts", draft.weights.fromContacts],
      ["mentions of you", draft.weights.mentionsMe],
      ["photos and video", draft.weights.hasMedia],
    ] as const
  )
    .filter(([, w]) => w > 0)
    .map(([label]) => label);
  lines.push(ranked.length === 0 ? "Newest first" : `Ranks by recency and ${ranked.join(", ")}`);
  lines.push(`Looks back ${draft.windowDays} day${draft.windowDays === 1 ? "" : "s"}`);
  if (draft.forMe) lines.push("Only what is about you");
  if (draft.mediaOnly) lines.push("Only with photos or video");
  if (draft.minReactions > 0) lines.push(`At least ${draft.minReactions} reactions`);
  if (draft.hideOwn) lines.push("Hides your own messages");
  if (draft.hideWords.length > 0) lines.push(`Hides ${draft.hideWords.length} words`);
  if (draft.onlyWords.length > 0) lines.push(`Only messages with: ${draft.onlyWords.join(", ")}`);
  return lines;
}
