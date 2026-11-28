// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the signed-in account's published protection lists (ADR-0101 section
 * 10, the seal announcement; `docs/ux/identity-recovery.md` 19.1d): one
 * reactive value for the page.
 *
 * "A phrase is saved" and "a passkey keeps the chats" are no server flags:
 * they are what the account's own current announcement lists. Every device
 * reads it from the bundle the account publishes (`seal-runtime.ts` writes
 * here after each read and after each announcement this device publishes).
 *
 * Nothing here is a secret: an announcement is a public artefact.
 */
import { createSignal } from "solid-js";

import { registerAccountMemory } from "../account-memory";

/** What the account's current seal announcement lists. */
export interface OwnSealFacts {
  /** A password seal is listed (every announcement has exactly one). */
  readonly password: boolean;
  /** A phrase seal is listed: the recovery phrase was saved. */
  readonly phrase: boolean;
  /** Lower-case hex of the 16-byte tag of every passkey seal listed. */
  readonly passkeyTagsHex: readonly string[];
  /** The announcement's `seq`, as a decimal string. */
  readonly seq: string;
}

interface Held {
  readonly accountIdB64: string;
  readonly facts: OwnSealFacts;
}

const [held, setHeld] = createSignal<Held | null>(null);

/** Reactive: the facts of `accountIdB64`, or `null` while they are not
 *  known (not read yet, or the account published no announcement). */
export function ownSealFacts(accountIdB64: string | null): OwnSealFacts | null {
  const h = held();
  return h !== null && h.accountIdB64 === accountIdB64 ? h.facts : null;
}

/** Record what was just read or published for the account. */
export function adoptOwnSealFacts(accountIdB64: string, facts: OwnSealFacts | null): void {
  setHeld(facts === null ? null : { accountIdB64, facts });
}

/**
 * What keeps the account's chats through a key reset (UX 19.1c status
 * line, 19.1d): the phrase when one is saved, else a passkey that keeps the
 * chats, else the password alone. `null` while the facts are not known:
 * nothing is claimed and no notice is shown.
 */
export type ChatsKeptBy = "phrase" | "passkey" | "password";

export function chatsKeptBy(facts: OwnSealFacts | null): ChatsKeptBy | null {
  if (facts === null) return null;
  if (facts.phrase) return "phrase";
  if (facts.passkeyTagsHex.length > 0) return "passkey";
  return "password";
}

/** Whether the passkey with this 16-byte seal tag (lower-case hex) keeps the
 *  chats. `null` while the facts are not known. */
export function passkeyKeepsChats(facts: OwnSealFacts | null, tagHex: string): boolean | null {
  if (facts === null) return null;
  return facts.passkeyTagsHex.includes(tagHex);
}

// Facts held from before Forget device belong to the wiped account; a
// sign-out drops them too (they are read again at the next landing).
registerAccountMemory(
  "own-seal-facts",
  (ids) => {
    const h = held();
    if (h !== null && ids.includes(h.accountIdB64)) setHeld(null);
  },
  { signOut: true },
);
