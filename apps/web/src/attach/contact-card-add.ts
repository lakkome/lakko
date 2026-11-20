// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0092 section 7 / UX 10.2 — `Add contact` on a shared contact card.
 * The card carries nothing that lets the receiver write to the person
 * described: the ordinary username lookup runs (so that person's "Who can
 * find me" decides), and a request goes out only when the account it finds
 * carries the card's key.
 */
import { base64Decode, base64Encode } from "../auth/account-id";
import type { SharedContactCard } from "../../wasm/lakko_ffi_wasm";

export type CardAddOutcome = "sent" | "changed" | "not-found";

export interface CardAddPorts {
  /** The ordinary username lookup: the account id, or `null` when the name
   *  cannot be found (gone, or the person is not findable by username). */
  lookup(username: string): Promise<Uint8Array | null>;
  /** The account's published keys. */
  identity(accountId: Uint8Array): Promise<{ signaturePubkeyB64: string; x25519PubkeyB64: string } | null>;
  /** Send the ordinary contact request to that bundle. */
  send(accountId: Uint8Array, signaturePubkey: Uint8Array, x25519Pubkey: Uint8Array): Promise<void>;
}

/** The decision, with the network behind ports so it is tested directly. */
export async function addContactFromCard(card: SharedContactCard, ports: CardAddPorts): Promise<CardAddOutcome> {
  const account = await ports.lookup(card.username);
  if (account === null) return "not-found";
  const bundle = await ports.identity(account);
  if (bundle === null) return "not-found";
  if (!sameKey(bundle.signaturePubkeyB64, card.accountKey)) return "changed";
  await ports.send(account, base64Decode(bundle.signaturePubkeyB64), base64Decode(bundle.x25519PubkeyB64));
  return "sent";
}

function sameKey(a: string, b: string): boolean {
  try {
    return base64Encode(base64Decode(a)) === base64Encode(base64Decode(b));
  } catch {
    return false;
  }
}
