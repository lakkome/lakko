// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 - what the receive loops do with a scoped-discovery envelope that
 * arrived in a conversation.
 *
 * - a **committer delegation** is kept, newest per (sender, added account),
 *   so a sweep this device commits can write the added person's Welcome;
 *   this account's own other devices included: one of them may propose an Add
 *   this device sweeps, and a device never receives its own group messages, so
 *   this is not an echo;
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
import { createSignal } from "solid-js";
import { base64Decode, base64Encode } from "../auth/account-id";
import { registerAccountMemory } from "../auth/account-memory";
import { journalHex } from "../ds/access";
import { loadWasm } from "../wasm";
import type { ScopedEnvelopeKind } from "./envelope-sniff";
import { fileDelegations, otherDeviceSender } from "./received-grants";
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
  if (!facts.senderResolved) return false;
  if (kind === "committer-delegation") return true;
  if (facts.fromSelf) return false;
  switch (kind) {
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

const [offersTick, setOffersTick] = createSignal(0);

/**
 * Bumped when a contact's offered record is new or changed. The publication
 * runtime re-runs on it, so the record is relayed to this account's other
 * contacts at once rather than at the next periodic check.
 */
export const introductionOffersTick = offersTick;

// The cache is what the next save starts from: a row cached before Forget
// device would be saved back over the wiped store at the account's next
// sign-in on this page. A sign-out drops it too (re-read on the next load).
registerAccountMemory(
  "reach-introductions",
  (ids) => {
    if (introductionsCache !== null && ids.includes(introductionsCache.account)) {
      introductionsCache = null;
    }
    introductionAsks.clear();
  },
  { signOut: true },
);

/** Group id base64 -> the sender (account key base64) of the newest profile
 *  request not looked at yet. */
const introductionAsks = new Map<string, string>();

/**
 * Someone other than this account sent a profile request into a conversation
 * (ADR-0060 section 4, "A device that holds nothing asks"). The publication
 * runtime decides whether it counts: it owns the row a reset is written to,
 * and it may be half way through a run that would save over it.
 */
export function noteIntroductionsAsk(groupIdB64: string, senderKeyB64: string): void {
  introductionAsks.set(groupIdB64, senderKeyB64);
  setOffersTick((n) => n + 1);
}

/** The asks noted since the last call. */
export function takeIntroductionAsks(): { readonly groupIdB64: string; readonly senderKeyB64: string }[] {
  const asks = [...introductionAsks].map(([groupIdB64, senderKeyB64]) => ({ groupIdB64, senderKeyB64 }));
  introductionAsks.clear();
  return asks;
}

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

let introductionsChain: Promise<unknown> = Promise.resolve();

/**
 * Read, change and write the kept introductions one at a time. Intake runs
 * fire-and-forget per envelope, and a contact sends its offer and its bundle
 * back to back: two unserialized writes each started from the row as it was
 * before the other, and the later one dropped what the earlier one kept.
 * `change` returns the new row, or `null` to write nothing.
 */
async function updateIntroductions(
  accountIdB64: string,
  change: (row: IntroductionsRow) => IntroductionsRow | null,
): Promise<boolean> {
  const run = introductionsChain.then(async () => {
    const next = change(await loadIntroductions(accountIdB64));
    if (next === null) return false;
    await saveIntroductions(accountIdB64, next);
    return true;
  });
  introductionsChain = run.catch(() => undefined);
  return run;
}

/**
 * The row without what the person known by `keysHex` (every key they have
 * had, account key hex) offered or relayed, or `null` when it holds nothing
 * of theirs. A removed contact's record is no longer relayed, and the records
 * they relayed are no longer searched.
 */
export function withoutIntroductionsFrom(
  row: IntroductionsRow,
  keysHex: readonly string[],
): IntroductionsRow | null {
  const drop = new Set(keysHex);
  const offers = Object.fromEntries(Object.entries(row.offers).filter(([k]) => !drop.has(k)));
  const bundles = Object.fromEntries(Object.entries(row.bundles).filter(([k]) => !drop.has(k)));
  const acks =
    row.acks === undefined ? undefined : Object.fromEntries(Object.entries(row.acks).filter(([k]) => !drop.has(k)));
  const same =
    Object.keys(offers).length === Object.keys(row.offers).length &&
    Object.keys(bundles).length === Object.keys(row.bundles).length &&
    (row.acks === undefined || Object.keys(acks ?? {}).length === Object.keys(row.acks).length);
  if (same) return null;
  return { ...row, offers, bundles, ...(acks === undefined ? {} : { acks }) };
}

/**
 * Forget what a removed contact offered and relayed (ADR-0060 section 4),
 * under every key they have had. The relay runs again, so the bundle this
 * account's other contacts hold is replaced. Never throws.
 */
export async function dropIntroductionsFrom(keysHex: readonly string[]): Promise<void> {
  const session = reachSession();
  if (session === null || keysHex.length === 0) return;
  try {
    const changed = await updateIntroductions(session.accountIdB64, (row) => withoutIntroductionsFrom(row, keysHex));
    if (changed) {
      // eslint-disable-next-line no-console
      console.info("[reach] a removed contact's introductions were dropped");
      setOffersTick((n) => n + 1);
    }
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[reach] dropping a removed contact's introductions failed", err instanceof Error ? err.name : "");
  }
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
  /** The group the envelope arrived in (journal id hex). Recorded with a
   *  committer-delegation envelope: one with no delegation authorises the
   *  unsigned Welcome for this group only (ADR-0060 section 6). */
  readonly scopeHex?: string;
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
  const fromSelf = senderHex === session.accountKeyHex;
  if (
    !keepsScopedEnvelope(args.kind, {
      senderResolved: senderHex !== null,
      fromSelf,
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
          fileDelegations(g, fromSelf ? otherDeviceSender(sender) : sender, addedHex, {
            envelopeB64: base64Encode(args.payload),
            receivedAtMs: Date.now(),
            ...(args.scopeHex === undefined ? {} : { scopeHex: args.scopeHex }),
          }),
        );
        return;
      }
      case "introduction-offer": {
        const record = wasm.decodeIntroductionOffer(args.payload);
        const recordB64 = base64Encode(record);
        let changed = false;
        await updateIntroductions(session.accountIdB64, (row) => {
          changed = row.offers[sender]?.recordB64 !== recordB64;
          return { ...row, offers: { ...row.offers, [sender]: { recordB64, receivedAtMs: Date.now() } } };
        });
        if (changed) setOffersTick((n) => n + 1);
        return;
      }
      case "introductions": {
        const bundle = wasm.decodeIntroductionsBundle(args.payload);
        const updated = bundle.updatedAtMs;
        bundle.free();
        const bundleB64 = base64Encode(args.payload);
        await updateIntroductions(session.accountIdB64, (row) => {
          const held = row.bundles[sender];
          if (held !== undefined && !wasm.bundleIsNewer(updated, BigInt(held.updatedAtMs))) return null;
          return { ...row, bundles: { ...row.bundles, [sender]: { bundleB64, updatedAtMs: updated.toString() } } };
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
