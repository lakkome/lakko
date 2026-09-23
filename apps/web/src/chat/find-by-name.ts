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
 * Pure of the UI: the two lookups arrive as functions, so this can be run by
 * a test - vitest has no jsdom and no Solid transform here.
 */

/** What a lookup turned up. */
export type FoundByName =
  | { readonly kind: "channel"; readonly channelIdHex: string; readonly handle: string }
  | { readonly kind: "person"; readonly username: string }
  | { readonly kind: "nothing" }
  | { readonly kind: "unreachable" };

/** The two namespaces, as the caller can ask them. Each resolves to `null`
 *  when the name is not in that namespace, and throws when the network is
 *  the problem - the difference matters, because one is an answer and the
 *  other is no answer at all. */
export interface NameLookups {
  readonly channel: (handle: string) => Promise<string | null>;
  readonly person: (username: string) => Promise<string | null>;
}

/** Strip a leading `@`, which people type and no namespace stores. */
export function bareName(typed: string): string {
  return typed.trim().replace(/^@+/, "");
}

/**
 * Whether this is worth spending a lookup on.
 *
 * The canonical username rule (`[a-z0-9_]`, 3-32 after normalisation) is the
 * same rule the blinder enforces, so anything it would refuse is refused
 * here first rather than costing a proof of work to be told no.
 */
export function looksLikeAName(typed: string): boolean {
  const name = bareName(typed).toLocaleLowerCase();
  return name.length >= 3 && name.length <= 32 && /^[a-z0-9_]+$/.test(name);
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
): Promise<FoundByName> {
  const name = bareName(typed);
  if (!looksLikeAName(name)) return { kind: "nothing" };
  let reachedTheServer = false;
  try {
    const channelIdHex = await lookups.channel(name);
    reachedTheServer = true;
    if (channelIdHex !== null) {
      return { kind: "channel", channelIdHex, handle: name };
    }
  } catch {
    // Fall through: the other namespace may still answer.
  }
  try {
    const username = await lookups.person(name);
    reachedTheServer = true;
    if (username !== null) return { kind: "person", username };
  } catch {
    // Handled below.
  }
  return reachedTheServer ? { kind: "nothing" } : { kind: "unreachable" };
}

/** What to say about a result. One sentence, and never a protocol word. */
export function findByNameMessage(result: FoundByName, typed: string): string {
  const at = `@${bareName(typed)}`;
  switch (result.kind) {
    case "nothing":
      return `Nothing found for ${at}.`;
    case "unreachable":
      return "Could not look that up. Check your connection and try again.";
    default:
      return "";
  }
}
