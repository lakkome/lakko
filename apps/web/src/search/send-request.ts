// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * **Add contact** on a search result: send the contact request with the key
 * that found the person (`docs/ux/who-can-find-me.md` section 6.1, ADR-0060
 * section 6).
 *
 * The person may have changed who can find them between the search and the
 * tap, so a result found through a grant is checked AGAIN right before the
 * request goes out. When the row's grant is refused, every other grant this
 * device holds for the person is tried, read fresh (a new one may have
 * arrived) and in the usual order, and the request goes with the first that
 * passes. None passing is **Request refused**, and nothing is sent (an
 * unsigned-looking write to a closed mailbox would be answered as stored and
 * silently dropped, which is worse than saying so). A check that could not be
 * made is a failure, not a refusal.
 *
 * The request goes out at once, with no introduction text (UX section 6.1:
 * no dialog, no form, no second field); it carries the sender's profile and
 * grants exactly as every other request does.
 *
 * Which send path:
 * - a person in Everyone with a verified username goes through the username
 *   path (`sendContactRequest`), which resolves the name again and runs the
 *   identity-key pin gate (ADR-0016) before sealing;
 * - anyone else (found by name, or through contacts of contacts) through the
 *   bundle path with the key cross-checked here: the key the identity bundle
 *   publishes for the account id must be the key the person was found as,
 *   or nothing is sent.
 */
import { base64Decode } from "../auth/account-id";
import { canAddContact, toHex, type PersonResult } from "./people";

export type SendOutcome =
  | { readonly kind: "sent" }
  /** `byReachCheck`: the DS reach check said no, so the kept grant no longer
   *  reaches them (`reach-verdicts.ts`). */
  | { readonly kind: "refused"; readonly byReachCheck?: boolean }
  /** `message` renders the failure in the current language. */
  | { readonly kind: "failed"; readonly message: () => string };

export interface SendDeps {
  /** The DS reach check of the person's account mailbox with a grant. */
  reachCheck(identityPk: Uint8Array, grantWireB64: string): Promise<boolean>;
  /** Every grant this device holds for the person now, in the order they are
   *  tried. Absent: only the row's. */
  heldGrantWires?(identityPk: Uint8Array): readonly string[];
  /** The identity bundle of an account: its account key and X25519 key. */
  identityFetch(
    accountId: Uint8Array,
  ): Promise<{ readonly signaturePub: Uint8Array; readonly x25519Pub: Uint8Array } | null>;
  /** ADR-0074 section 3.5a - whether the key the account publishes now is
   *  the PROVEN successor of `foundKey`. Absent: only an exact match sends. */
  provenPredecessor?(foundKey: Uint8Array, accountId: Uint8Array): Promise<boolean>;
  sendByUsername(username: string, intro: Uint8Array, grantWireB64: string | null): Promise<void>;
  sendToBundle(
    accountId: Uint8Array,
    signaturePub: Uint8Array,
    x25519Pub: Uint8Array,
    intro: Uint8Array,
    grantWireB64: string | null,
    /** ADR-0078 - the person's mailbox binding from their public record
     *  (standard base64), for an unsigned request to someone in Everyone. */
    openBindingB64?: string | null,
  ): Promise<void>;
  /** Whether a send failure means the person can no longer be reached this
   *  way (the username no longer resolves: they left Everyone). */
  isRefusal(err: unknown): boolean;
  /** How to render the sentence for any other failure. */
  errorMessage(err: unknown): () => string;
}

/** The introduction a request from search carries: none. */
export const NO_INTRO = new Uint8Array(0);

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** The first grant that still reaches the person: the row's, else any other
 *  held for them. `null` when every one was refused. Throws what the check
 *  throws: a check that could not be made is no refusal. */
async function reachingGrant(
  key: Uint8Array,
  rowGrant: string | null,
  deps: SendDeps,
): Promise<string | null> {
  const held = deps.heldGrantWires?.(key) ?? [];
  const wires = rowGrant === null ? held : [rowGrant, ...held.filter((w) => w !== rowGrant)];
  for (const wire of wires) {
    if (await deps.reachCheck(key, wire)) return wire;
  }
  return null;
}

/** Send the request. Never throws. */
export async function sendFoundRequest(person: PersonResult, deps: SendDeps): Promise<SendOutcome> {
  if (!canAddContact(person) || person.accountIdHex === null) return { kind: "refused" };
  try {
    const key = base64Decode(person.keyB64);
    let grant: string | null = null;
    if (person.reach === "grant") {
      grant = await reachingGrant(key, person.grantWireB64, deps);
      if (grant === null) return { kind: "refused", byReachCheck: true };
    }
    if (person.reach === "open" && person.username !== null) {
      await deps.sendByUsername(person.username, NO_INTRO, grant);
      return { kind: "sent" };
    }
    const accountId = hexToBytes(person.accountIdHex);
    const bundle = await deps.identityFetch(accountId);
    // No bundle, or a bundle for another key: the person as found cannot be
    // reached under this account id. Nothing is sent. One exception
    // (ADR-0074 section 3.5a): the person was found under a key their
    // published key PROVABLY superseded. It is the same person, and the
    // request goes to the published (current) keys, never to the old one.
    if (bundle === null) return { kind: "refused" };
    if (
      toHex(bundle.signaturePub) !== person.keyHex &&
      !((await deps.provenPredecessor?.(key, accountId)) ?? false)
    ) {
      return { kind: "refused" };
    }
    await deps.sendToBundle(
      accountId,
      bundle.signaturePub,
      bundle.x25519Pub,
      NO_INTRO,
      grant,
      person.reach === "open" ? (person.openBindingB64 ?? null) : null,
    );
    return { kind: "sent" };
  } catch (err) {
    if (deps.isRefusal(err)) return { kind: "refused" };
    return { kind: "failed", message: deps.errorMessage(err) };
  }
}

/**
 * `docs/ux/who-can-find-me.md` section 6.4 - the same order as Message on a
 * contact card: a request the inbox refused (no grant reaches the person)
 * goes inside a group both people are in, the one with the newest message.
 * `groupIdB64` names the group it went into, `null` when it did not. Never
 * throws: a failed send inside the group is a failed send.
 */
export async function withSharedGroupFallback(
  outcome: SendOutcome,
  deps: {
    readonly sharedGroups: () => Promise<readonly { readonly groupIdB64: string; readonly newestMs: number }[]>;
    readonly sendInGroup: (groupIdB64: string) => Promise<void>;
    readonly errorMessage: (err: unknown) => () => string;
  },
): Promise<{ readonly outcome: SendOutcome; readonly groupIdB64: string | null }> {
  if (outcome.kind !== "refused") return { outcome, groupIdB64: null };
  try {
    let best: { readonly groupIdB64: string; readonly newestMs: number } | null = null;
    for (const g of await deps.sharedGroups()) if (best === null || g.newestMs > best.newestMs) best = g;
    if (best === null) return { outcome, groupIdB64: null };
    await deps.sendInGroup(best.groupIdB64);
    return { outcome: { kind: "sent" }, groupIdB64: best.groupIdB64 };
  } catch (err) {
    return { outcome: { kind: "failed", message: deps.errorMessage(err) }, groupIdB64: null };
  }
}
