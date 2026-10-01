// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The states of **Search people** and what each one says
 * (`docs/ux/who-can-find-me.md` section 6.1, copy verbatim).
 *
 * The rule this module exists to hold: "not found" is an ANSWER, and only a
 * search whose every question was answered may give it. A network search
 * asks several questions (the username, the name, the channel handle), and
 * when one of them got no answer and nothing was found, the truth is "could
 * not search" - telling someone their friend does not exist because the
 * connection dropped is the worst thing this can do.
 */
import { anonTokenQuotaExhausted } from "../tokens/anon-token-pool";
import { LOOKUP_ALLOWANCE_USED } from "../tokens/token-copy";

/** Not found - identical whether the person does not exist or does not let
 *  the searcher find them. */
export const SEARCH_NOT_FOUND = "No one found. Check the spelling, or ask them for an invite link.";
/** Network failure. Never "not found". */
export const SEARCH_FAILED = "Couldn't search. Check your connection and try again.";
/** The person changed their setting after they were found. */
export const REQUEST_REFUSED = "Couldn't send the request. They may have changed who can find them.";

/** How one question of a search ended. */
export type LegOutcome = "answered" | "failed" | "allowance-used";

export type PeopleSearchState =
  | { readonly kind: "idle" }
  | { readonly kind: "searching" }
  | { readonly kind: "found" }
  | { readonly kind: "not-found" }
  | { readonly kind: "failed" }
  /** ADR-0054: each lookup spends an anonymous token and today's are used. */
  | { readonly kind: "allowance-used" };

/** The outcome of a failed question. */
export function legOutcomeOf(err: unknown): LegOutcome {
  return anonTokenQuotaExhausted(err) !== null ? "allowance-used" : "failed";
}

/**
 * Fold the questions a search asked into its state. Anything found is shown,
 * whatever else failed; nothing found is "not found" only when every
 * question was answered.
 */
export function foldSearch(foundCount: number, legs: readonly LegOutcome[]): PeopleSearchState {
  if (foundCount > 0) return { kind: "found" };
  if (legs.includes("allowance-used")) return { kind: "allowance-used" };
  if (legs.includes("failed")) return { kind: "failed" };
  return { kind: "not-found" };
}

/** The line under the row for a state; `null` when the state says nothing
 *  in words (idle, searching - the spinner says it - and found). */
export function searchStateMessage(state: PeopleSearchState): string | null {
  switch (state.kind) {
    case "not-found":
      return SEARCH_NOT_FOUND;
    case "failed":
      return SEARCH_FAILED;
    case "allowance-used":
      return LOOKUP_ALLOWANCE_USED();
    default:
      return null;
  }
}
