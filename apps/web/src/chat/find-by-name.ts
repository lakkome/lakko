// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Looking a name up on the network, from the conversation list.
 *
 * `docs/ux/navigation.md` section 4. A group and a channel are kinds of
 * conversation, not kinds of contact, so finding one belongs in Chats -
 * Contacts is about people. And finding a person to talk to starts a
 * conversation too, so it belongs here as well.
 *
 * **There is one lookup, not one per kind.** `@username` and a channel's
 * `@handle` are the same mechanism over two namespaces: blind the name, ask,
 * get an exact answer or nothing. A group handle would be a third namespace
 * and the same mechanism again. The person typing does not know which
 * namespace the name they were given lives in, and should not have to - so
 * this asks both and reports what it found.
 *
 * **It cannot be fuzzy.** The server evaluates the blinded name without
 * learning it (invariant #5), which answers "does this exact name exist" and
 * cannot answer "which names start with these letters", because a prefix
 * search is the server learning the prefix. The caller must offer this as
 * "look up the whole name", never as a list of partial matches.
 *
 * **The gate in front of it is not here.** Whether a typed string could be a
 * name at all is `looks_like_a_name` in `crates/lakko-ui-rules`, shared with
 * iOS and Android; what this module owns is asking the two namespaces and
 * telling the difference between "no such name" and "no answer".
 *
 * **Superseded in the Chats field by `search/people-search.ts`** (ADR-0060,
 * `docs/ux/who-can-find-me.md` section 6.1): the Search people row asks the
 * channel namespace the same way and replaces the person half with the
 * username, name and contacts-of-contacts search, whose results need the
 * account id and the OPRF output this function does not return. This one is
 * kept for its tests (`tokens/token-copy.test.ts` pins its allowance copy).
 *
 * Pure of the UI: the two lookups and the shared rules arrive as arguments,
 * so this can be run by a test - vitest has no jsdom, no Solid transform and
 * no wasm here.
 */
import type { UiRules } from "./ui-rules";
import { anonTokenQuotaExhausted } from "../tokens/anon-token-pool";
import { LOOKUP_ALLOWANCE_USED } from "../tokens/token-copy";

/** What a lookup turned up. */
export type FoundByName =
  | { readonly kind: "channel"; readonly channelIdHex: string; readonly handle: string }
  | { readonly kind: "person"; readonly username: string }
  | { readonly kind: "nothing" }
  | { readonly kind: "unreachable" }
  /** ADR-0054 S3: each lookup spends one anonymous token, and today's are
   *  used up. Not "nothing": the name may well exist. */
  | { readonly kind: "allowance-used" };

/** The two namespaces, as the caller can ask them. Each resolves to `null`
 *  when the name is not in that namespace, and throws when the network is
 *  the problem - the difference matters, because one is an answer and the
 *  other is no answer at all. */
export interface NameLookups {
  readonly channel: (handle: string) => Promise<string | null>;
  readonly person: (username: string) => Promise<string | null>;
}

/**
 * Ask both namespaces for one name.
 *
 * The channel goes first only because its answer is actionable on its own -
 * subscribing needs nothing else - while a person leads into the contact
 * request flow. Nothing about correctness depends on the order: a name
 * cannot be in both namespaces, since they are separate registries.
 *
 * A network failure is `unreachable` rather than `nothing`: telling someone
 * their friend does not exist because the connection dropped is the worst
 * thing this function could do.
 */
export async function findByName(
  typed: string,
  lookups: NameLookups,
  rules: UiRules,
): Promise<FoundByName> {
  const name = rules.bareName(typed);
  if (!rules.looksLikeAName(name)) return { kind: "nothing" };
  let reachedTheServer = false;
  let allowanceUsed = false;
  try {
    const channelIdHex = await lookups.channel(name);
    reachedTheServer = true;
    if (channelIdHex !== null) {
      return { kind: "channel", channelIdHex, handle: name };
    }
  } catch (err) {
    // Fall through: the other namespace may still answer.
    if (anonTokenQuotaExhausted(err) !== null) allowanceUsed = true;
  }
  if (!allowanceUsed) {
    try {
      const username = await lookups.person(name);
      reachedTheServer = true;
      if (username !== null) return { kind: "person", username };
    } catch (err) {
      if (anonTokenQuotaExhausted(err) !== null) allowanceUsed = true;
    }
  }
  if (allowanceUsed) return { kind: "allowance-used" };
  return reachedTheServer ? { kind: "nothing" } : { kind: "unreachable" };
}

/** What to say about a result. One sentence, and never a protocol word. */
export function findByNameMessage(
  result: FoundByName,
  typed: string,
  rules: UiRules,
): string {
  const at = `@${rules.bareName(typed)}`;
  switch (result.kind) {
    case "nothing":
      return `Nothing found for ${at}.`;
    case "unreachable":
      return "Could not look that up. Check your connection and try again.";
    case "allowance-used":
      return LOOKUP_ALLOWANCE_USED;
    default:
      return "";
  }
}
