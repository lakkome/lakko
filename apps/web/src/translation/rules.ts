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
    readonly isOwn: boolean;
    readonly detected: Detection | null;
    readonly engineForDetected: EngineState;
    readonly anyEngine: boolean;
    readonly reading: readonly string[];
  },
): Offer {
  const answer = r.translationOffered(
    input.enabled,
    input.hasText,
    input.isOwn,
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
