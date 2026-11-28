// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0082 section 6 — the decisions the three clients make alike, reached
 * over wasm from `lakko-ui-rules::translation`.
 *
 * Nothing here restates a rule: each function maps this client's state onto
 * the shared rule's arguments and maps its answer back. A strict subset of the
 * wasm namespace, so a test can hand in the real module (vitest loads it
 * through the node glue) or a fake.
 */

import { createSignal } from "solid-js";

import { loadWasm, type Wasm } from "../wasm";
import type { Detection, EngineState } from "./engines";
import type { ConversationLanguagesRecord } from "./result-store";

export type TranslationRules = Pick<
  Wasm,
  | "TranslationEngineState"
  | "TranslateOffer"
  | "translationOffered"
  | "conversationBarLanguage"
  | "transcriptOffered"
  | "storedResultIsCurrent"
  | "canonicalLanguageTag"
  | "sameLanguage"
  | "readsLanguage"
  | "translationDetectionConfidentPermille"
  | "translationBarWindow"
  | "conversationDetection"
  | "ConversationLanguages"
  | "resolveTranslationSource"
  | "orderPickerLanguages"
>;

const [rules, setRules] = createSignal<TranslationRules | null>(null);
// Primed at import, the `chat/ui-rules.ts` pattern: until wasm has loaded a
// caller sees `null` and offers nothing, then re-renders.
if (typeof window !== "undefined") {
  void loadWasm()
    .then((wasm) => setRules(() => wasm))
    .catch(() => undefined);
}

/** The shared rules, or `null` until wasm has loaded (a Solid signal). */
export function translationRules(): TranslationRules | null {
  return rules();
}

/** For tests: install a rules object. */
export function setTranslationRulesForTests(r: TranslationRules | null): void {
  setRules(() => r);
}

export type Offer = "none" | "translate" | "ask";

function engineArg(r: TranslationRules, s: EngineState): number {
  switch (s) {
    case "ready":
      return r.TranslationEngineState.Ready;
    case "needs-pack":
      return r.TranslationEngineState.NeedsPack;
    default:
      return r.TranslationEngineState.Unavailable;
  }
}

/** UX 3.1 — what the message menu offers for one message. */
export function offerFor(
  r: TranslationRules,
  input: {
    readonly enabled: boolean;
    readonly hasText: boolean;
    readonly detected: Detection | null;
    readonly engineForDetected: EngineState;
    readonly anyEngine: boolean;
    readonly reading: readonly string[];
  },
): Offer {
  const answer = r.translationOffered(
    input.enabled,
    input.hasText,
    input.detected?.tag ?? "",
    input.detected?.permille ?? 0,
    engineArg(r, input.engineForDetected),
    input.anyEngine,
    [...input.reading],
  );
  if (answer === r.TranslateOffer.Translate) return "translate";
  if (answer === r.TranslateOffer.TranslateAskLanguage) return "ask";
  return "none";
}

/** The shared rule's object for a stored record (the caller frees it). */
function languagesOf(r: TranslationRules, record: ConversationLanguagesRecord) {
  return new r.ConversationLanguages(
    record.picks.map((p) => p.tag),
    Float64Array.from(record.picks, (p) => p.atMs),
    Float64Array.from(record.detections, (d) => d.messageId),
    record.detections.map((d) => d.tag),
  );
}

/** A stored record from the shared rule's object. */
function recordOf(l: InstanceType<TranslationRules["ConversationLanguages"]>): ConversationLanguagesRecord {
  const pickTags = l.pickTags;
  const pickAt = l.pickAtMs;
  const ids = l.detectionIds;
  const tags = l.detectionTags;
  return {
    picks: pickTags.map((tag: string, i: number) => ({ tag, atMs: pickAt[i] ?? 0 })),
    detections: Array.from(ids, (messageId, i) => ({ messageId, tag: tags[i] ?? "" })),
  };
}

/** The record with a pick (`Not X?` or the picker) of `tag` at `atMs`. */
export function withLanguagePick(
  r: TranslationRules,
  record: ConversationLanguagesRecord,
  tag: string,
  atMs: number,
): ConversationLanguagesRecord {
  const l = languagesOf(r, record);
  const next = l.withPick(tag, atMs);
  try {
    return recordOf(next);
  } finally {
    next.free();
    l.free();
  }
}

/** The record with message `messageId` confidently detected as `tag`. */
export function withLanguageDetection(
  r: TranslationRules,
  record: ConversationLanguagesRecord,
  messageId: number,
  tag: string,
): ConversationLanguagesRecord {
  const l = languagesOf(r, record);
  const next = l.withDetection(messageId, tag);
  try {
    return recordOf(next);
  } finally {
    next.free();
    l.free();
  }
}

/** The conversation's usual language (latest pick, else most detected). */
export function usualLanguage(r: TranslationRules, record: ConversationLanguagesRecord): string | null {
  const l = languagesOf(r, record);
  try {
    return l.usual() ?? null;
  } finally {
    l.free();
  }
}

/**
 * ADR-0082 note of 2026-11-15 — the language an unsure message is taken to be
 * in: a picked close candidate, else the most detected one, else the
 * detection; a confident detection stands.
 */
export function conversationDetectionFor(
  r: TranslationRules,
  detected: Detection | null,
  record: ConversationLanguagesRecord,
): Detection | null {
  if (detected === null) return null;
  const l = languagesOf(r, record);
  const chosen = r.conversationDetection(detected.tag, detected.permille, [...(detected.alternatives ?? [])], l);
  l.free();
  if (chosen === undefined) return detected;
  try {
    const tag = chosen.tag;
    const permille = chosen.confidencePermille;
    return detected.alternatives === undefined ? { tag, permille } : { tag, permille, alternatives: detected.alternatives };
  } finally {
    chosen.free();
  }
}

/** What the line under a message names and what `Translate` starts from. */
export interface SourceResolution {
  /** The language the line names; `null` for no line. */
  readonly lineTag: string | null;
  readonly linePermille: number;
  /** Whether the line carries `Translate`; `false` is a name alone. */
  readonly lineCanTranslate: boolean;
  /** The language `Translate` starts from; `null` opens the picker. */
  readonly translateSource: string | null;
  /** The guess the picker marks. */
  readonly guess: string | null;
}

/**
 * ADR-0082 note of 2026-11-15 — the line and `Translate` from one shared rule
 * (`resolveTranslationSource`), so they never name different languages.
 * `translatable` lists the languages the engine answered it can translate
 * from into `target`.
 */
export function resolveSourceFor(
  r: TranslationRules,
  detected: Detection | null,
  record: ConversationLanguagesRecord,
  target: string,
  translatable: readonly string[],
): SourceResolution {
  const l = languagesOf(r, record);
  const res = r.resolveTranslationSource(
    detected?.tag,
    detected?.permille ?? 0,
    [...(detected?.alternatives ?? [])],
    l,
    target,
    [...translatable],
  );
  l.free();
  try {
    return {
      lineTag: res.lineTag ?? null,
      linePermille: res.lineConfidencePermille,
      lineCanTranslate: res.lineCanTranslate,
      translateSource: res.translateSource ?? null,
      guess: res.guess ?? null,
    };
  } finally {
    res.free();
  }
}

/** The detection's close candidates: the detected language, then the others. */
export function candidateTags(detected: Detection | null): string[] {
  return detected === null ? [] : [detected.tag, ...(detected.alternatives ?? [])];
}

/** `Not X?` — the picker's languages with the message's close candidates first. */
export function orderPickerLanguagesFor(
  r: TranslationRules,
  languages: readonly string[],
  candidates: readonly string[],
): string[] {
  if (candidates.length === 0) return [...languages];
  return r.orderPickerLanguages([...languages], [...candidates]);
}

/** UX 3.6 — the language the conversation bar names, before the engine is asked. */
export function barLanguageFor(
  r: TranslationRules,
  input: {
    readonly enabled: boolean;
    readonly autoTranslateOn: boolean;
    /** Newest first. */
    readonly recent: readonly Detection[];
    readonly dismissed: readonly string[];
    readonly reading: readonly string[];
  },
): string | null {
  return (
    r.conversationBarLanguage(
      input.enabled,
      input.autoTranslateOn,
      input.recent.map((d) => d.tag),
      Uint16Array.from(input.recent.map((d) => d.permille)),
      [...input.dismissed],
      [...input.reading],
    ) ?? null
  );
}

/** UX 4.1 — whether a voice or video message gets the transcript button. */
export function transcriptOfferedFor(
  r: TranslationRules,
  enabled: boolean,
  hasAudio: boolean,
  engine: EngineState,
): boolean {
  return r.transcriptOffered(enabled, hasAudio, engineArg(r, engine));
}

/** Whether a stored result still describes the message. */
export function isCurrent(
  r: TranslationRules,
  storedLastOpId: number,
  currentLastOpId: number,
  storedTarget: string,
  currentTarget: string,
): boolean {
  return r.storedResultIsCurrent(
    BigInt(storedLastOpId),
    BigInt(currentLastOpId),
    storedTarget,
    currentTarget,
  );
}
