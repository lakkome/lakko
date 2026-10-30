// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The states of the People part of the search field and what each one says
 * (`docs/ux/who-can-find-me.md` section 6.1, copy verbatim).
 *
 * The rule this module exists to hold: "not found" is an ANSWER, and only a
 * search whose every question was answered may give it. A network search
 * asks several questions (the username, the name, the channel handle), and
 * when one of them got no answer and nothing was found, the truth is "could
 * not search" - telling someone their friend does not exist because the
 * connection dropped is the worst thing this can do.
 */
import { t } from "../l10n";
import { anonTokenPoolEmpty, anonTokenQuotaExhausted } from "../tokens/anon-token-pool";

/** Not found - identical whether the person does not exist or does not let
 *  the searcher find them. */
export const SEARCH_NOT_FOUND = (): string => t("search.people.not_found");
/** Network failure. Never "not found". */
export const SEARCH_FAILED = (): string => t("search.people.failed");
/** Searching spends anonymous tokens and they ran out. Never "not found",
 *  never the network line. */
export const SEARCH_OUT_OF_SEARCHES = (): string => t("search.people.out_of_searches");
/** The person changed their setting after they were found. */
export const REQUEST_REFUSED = (): string => t("search.people.request_refused");
/** A request to a found person failed for any other reason. */
export const REQUEST_FAILED = (): string => t("search.people.request_failed");
/** What a found person's row reads once the request went. */
export const REQUEST_SENT = (): string => t("search.people.request_sent");

/** How one question of a search ended. */
export type LegOutcome = "answered" | "failed" | "allowance-used";

export type PeopleSearchState =
  | { readonly kind: "idle" }
  | { readonly kind: "searching" }
  | { readonly kind: "found" }
  | { readonly kind: "not-found" }
  | { readonly kind: "failed" }
  /** ADR-0054: each lookup spends an anonymous token and there is none left
   *  (the daily allowance is used, or the device's pool is empty until its
   *  next scheduled refill). */
  | { readonly kind: "allowance-used" };

/** The outcome of a failed question: out of tokens (the issuer's quota, or
 *  an empty pool with no refill running) is its own outcome. */
export function legOutcomeOf(err: unknown): LegOutcome {
  return anonTokenQuotaExhausted(err) !== null || anonTokenPoolEmpty(err) ? "allowance-used" : "failed";
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

/** The line under People for a state with `personRows` rows standing there:
 *  "not found" would contradict a row, so it is said only over none. */
export function peopleLineOf(state: PeopleSearchState, personRows: number): string | null {
  if (state.kind === "not-found" && personRows > 0) return null;
  return searchStateMessage(state);
}

/** The line under the row for a state; `null` when the state says nothing
 *  in words (idle, searching - the spinner says it - and found). */
export function searchStateMessage(state: PeopleSearchState): string | null {
  switch (state.kind) {
    case "not-found":
      return SEARCH_NOT_FOUND();
    case "failed":
      return SEARCH_FAILED();
    case "allowance-used":
      return SEARCH_OUT_OF_SEARCHES();
    default:
      return null;
  }
}
