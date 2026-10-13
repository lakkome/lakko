// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 - what the receive loops do with a scoped-discovery envelope that
 * arrived in a conversation.
 *
 * - a **committer delegation** is kept, newest per (sender, added account),
 *   so a sweep this device commits can write the added person's Welcome;
 * - an **introduction offer** (a contact's record for contacts of contacts)
 *   is kept only from the other participant of a one-to-one, newest per
 *   contact;
 * - an **introductions bundle** (the records a contact relays) likewise, newest
 *   per contact by `bundleIsNewer`;
 * - **Visibility**, **ReachState** and the ADR-0057 **RegistrarSeed** belong
 *   to other carriers; in a conversation they are dropped.
 *
 * Every sender is the account key resolved from the MLS sender leaf at
 * receipt (C-68); an unresolvable sender is dropped.
 */
import { base64Decode, base64Encode } from "../auth/account-id";
import { journalHex } from "../ds/access";
import { loadWasm } from "../wasm";
import type { ScopedEnvelopeKind } from "./envelope-sniff";
import { fileDelegations } from "./received-grants";
import { resolveAccountKeyHex } from "../contacts/address-resolver";
import { reachSession } from "./reach-service";
import { loadReachRow, saveReachRow } from "./reach-store";

/** Kept introduction records and bundles, per contact (account key hex). */
export interface IntroductionsRow {
  /** The record each contact offered for their own 2.1 publication. */
  readonly offers: Readonly<Record<string, { readonly recordB64: string; readonly receivedAtMs: number }>>;
  /** The bundle each contact relayed. */
  readonly bundles: Readonly<Record<string, { readonly bundleB64: string; readonly updatedAtMs: string }>>;
  /** Contacts that acknowledged our current record (spreading state). */
  readonly acks?: Readonly<Record<string, number>>;
}

export const EMPTY_INTRODUCTIONS: IntroductionsRow = { offers: {}, bundles: {} };

/** The pure decision: whether an envelope of `kind` is kept from this sender
 *  in this conversation. */
export function keepsScopedEnvelope(
  kind: ScopedEnvelopeKind,
  facts: { readonly senderResolved: boolean; readonly fromSelf: boolean; readonly oneToOne: boolean },
): boolean {
  if (!facts.senderResolved || facts.fromSelf) return false;
  switch (kind) {
    case "committer-delegation":
      return true;
    case "introduction-offer":
    case "introductions":
      return facts.oneToOne;
    case "visibility":
    case "reach-state":
    case "registrar-seed":
      return false;
  }
}

export function decodeIntroductionsRow(json: string | null): IntroductionsRow {
  if (json === null) return EMPTY_INTRODUCTIONS;
  try {
    const raw = JSON.parse(json) as Partial<IntroductionsRow> & { v?: number };
    if (raw.v !== 1) return EMPTY_INTRODUCTIONS;
    return {
      offers: raw.offers ?? {},
      bundles: raw.bundles ?? {},
      ...(raw.acks === undefined ? {} : { acks: raw.acks }),
    };
  } catch {
    return EMPTY_INTRODUCTIONS;
  }
}

let introductionsCache: { account: string; row: IntroductionsRow } | null = null;

/** The kept introductions of the signed-in account. */
export async function loadIntroductions(accountIdB64: string): Promise<IntroductionsRow> {
  if (introductionsCache?.account === accountIdB64) return introductionsCache.row;
  const row = decodeIntroductionsRow(await loadReachRow(accountIdB64, "introductions"));
  introductionsCache = { account: accountIdB64, row };
  return row;
}

async function saveIntroductions(accountIdB64: string, row: IntroductionsRow): Promise<void> {
  introductionsCache = { account: accountIdB64, row };
  await saveReachRow(accountIdB64, "introductions", JSON.stringify({ v: 1, ...row }));
}

/**
 * Handle one envelope. `oneToOne` is whether the conversation holds exactly
 * two people. Never throws.
 */
export async function intakeScopedEnvelope(args: {
  readonly kind: ScopedEnvelopeKind;
  readonly payload: Uint8Array;
  readonly senderKeyB64: string | undefined;
  readonly oneToOne: boolean;
}): Promise<void> {
  const session = reachSession();
  if (session === null) return;
  let senderHex: string | null = null;
  try {
    // ADR-0074 section 3.5a rule 2 - the sender as a PERSON: a leaf bound to
    // a proven predecessor (this account's own other devices included) is
    // filed, and recognised as self, under the current key.
    if (args.senderKeyB64 !== undefined) {
      senderHex = resolveAccountKeyHex(journalHex(base64Decode(args.senderKeyB64)));
    }
  } catch {
    senderHex = null;
  }
  if (
    !keepsScopedEnvelope(args.kind, {
      senderResolved: senderHex !== null,
      fromSelf: senderHex === session.accountKeyHex,
      oneToOne: args.oneToOne,
    }) ||
    senderHex === null
  ) {
    return;
  }
  const sender = senderHex;
  try {
    const wasm = await loadWasm();
    switch (args.kind) {
      case "committer-delegation": {
        const env = wasm.decodeCommitterDelegationEnvelope(args.payload);
        const addedHex = journalHex(env.addedAccountKey);
        env.free();
        await session.updateReceivedGrants((g) =>
          fileDelegations(g, sender, addedHex, {
            envelopeB64: base64Encode(args.payload),
            receivedAtMs: Date.now(),
          }),
        );
        return;
      }
      case "introduction-offer": {
        const record = wasm.decodeIntroductionOffer(args.payload);
        const row = await loadIntroductions(session.accountIdB64);
        await saveIntroductions(session.accountIdB64, {
          ...row,
          offers: { ...row.offers, [sender]: { recordB64: base64Encode(record), receivedAtMs: Date.now() } },
        });
        return;
      }
      case "introductions": {
        const bundle = wasm.decodeIntroductionsBundle(args.payload);
        const updated = bundle.updatedAtMs;
        bundle.free();
        const row = await loadIntroductions(session.accountIdB64);
        const held = row.bundles[sender];
        if (held !== undefined && !wasm.bundleIsNewer(updated, BigInt(held.updatedAtMs))) return;
        await saveIntroductions(session.accountIdB64, {
          ...row,
          bundles: {
            ...row.bundles,
            [sender]: { bundleB64: base64Encode(args.payload), updatedAtMs: updated.toString() },
          },
        });
        return;
      }
      default:
        return;
    }
  } catch (err) {
    // A malformed envelope authorises and shows nothing.
    // eslint-disable-next-line no-console
    console.warn(`[reach] ${args.kind} envelope dropped`, err instanceof Error ? err.message : "");
  }
}
