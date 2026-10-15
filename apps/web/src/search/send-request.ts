// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * **Add contact** on a search result: send the contact request with the key
 * that found the person (`docs/ux/who-can-find-me.md` section 6.1, ADR-0060
 * section 6).
 *
 * The person may have changed who can find them between the search and the
 * tap, so a result found through a grant is checked AGAIN right before the
 * request goes out: a check that says no is **Request refused**, and nothing
 * is sent (an unsigned-looking write to a closed mailbox would be answered as
 * stored and silently dropped, which is worse than saying so).
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
  | { readonly kind: "refused" }
  /** `message` renders the failure in the current language. */
  | { readonly kind: "failed"; readonly message: () => string };

export interface SendDeps {
  /** The DS reach check of the person's account mailbox with a grant. */
  reachCheck(identityPk: Uint8Array, grantWireB64: string): Promise<boolean>;
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

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** Send the request. Never throws. */
export async function sendFoundRequest(
  person: PersonResult,
  intro: Uint8Array,
  deps: SendDeps,
): Promise<SendOutcome> {
  if (!canAddContact(person) || person.accountIdHex === null) return { kind: "refused" };
  const grant = person.reach === "grant" ? person.grantWireB64 : null;
  try {
    const key = base64Decode(person.keyB64);
    if (person.reach === "grant") {
      if (grant === null || !(await deps.reachCheck(key, grant))) return { kind: "refused" };
    }
    if (person.reach === "open" && person.username !== null) {
      await deps.sendByUsername(person.username, intro, grant);
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
      intro,
      grant,
      person.reach === "open" ? (person.openBindingB64 ?? null) : null,
    );
    return { kind: "sent" };
  } catch (err) {
    if (deps.isRefusal(err)) return { kind: "refused" };
    return { kind: "failed", message: deps.errorMessage(err) };
  }
}
