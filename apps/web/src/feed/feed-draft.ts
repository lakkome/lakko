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
import { joinList, t } from "../l10n";

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
export function blankDraft(core: FeedDraftCore, name = t("feed.editor.default_name")): FeedDraft {
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
      return t("feed.problem.name");
    case "noKinds":
      return t("feed.problem.no_kinds");
    case "weightOutOfRange":
      return t("feed.problem.weight");
    case "windowOutOfRange":
      return t("feed.problem.window");
    case "conversations":
      return t("feed.problem.conversations");
    case "strippedWithSources":
      return t("feed.problem.stripped");
    case "words":
      return t("feed.problem.words");
    case "tooLarge":
      return t("feed.problem.too_large");
    default:
      return t("feed.problem.generic");
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
  lines.push(
    t(
      draft.groups && draft.channels
        ? "feed.summary.sources.both"
        : draft.groups
          ? "feed.summary.sources.groups"
          : "feed.summary.sources.channels",
    ),
  );
  if (draft.includeMuted) lines.push(t("feed.summary.include_muted"));
  const ranked = (
    [
      ["feed.summary.factor.reactions", draft.weights.reactions],
      ["feed.summary.factor.replies", draft.weights.replies],
      ["feed.summary.factor.contacts", draft.weights.fromContacts],
      ["feed.summary.factor.mentions", draft.weights.mentionsMe],
      ["feed.summary.factor.media", draft.weights.hasMedia],
    ] as const
  )
    .filter(([, w]) => w > 0)
    .map(([key]) => t(key));
  lines.push(
    ranked.length === 0
      ? t("feed.summary.newest_first")
      : t("feed.summary.ranks_by", { factors: joinList(ranked) }),
  );
  lines.push(t("feed.summary.look_back", { count: draft.windowDays }));
  if (draft.forMe) lines.push(t("feed.summary.for_me"));
  if (draft.mediaOnly) lines.push(t("feed.editor.media_only"));
  if (draft.minReactions > 0) lines.push(t("feed.min_reactions_count", { count: draft.minReactions }));
  if (draft.hideOwn) lines.push(t("feed.summary.hide_own"));
  if (draft.hideWords.length > 0) lines.push(t("feed.summary.hide_words", { count: draft.hideWords.length }));
  if (draft.onlyWords.length > 0) lines.push(t("feed.summary.only_words", { words: joinList(draft.onlyWords) }));
  return lines;
}
