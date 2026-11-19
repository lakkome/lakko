// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 phase 2 section 4 - the re-admitter: a device that holds a
 * conversation at the epoch a request names brings the device that asked
 * back, by one commit that replaces its old leaf with the KeyPackage it sent.
 *
 * ## Where a request arrives
 *
 * - **Route 1** - a DeviceGroup service envelope from another device of this
 *   account (`DeviceGroupSyncContext`).
 * - **Route 2** - a sealed mailbox row of kind `ConversationReentry` from the
 *   other person of a one-to-one (`MailboxContext`).
 * - **Route 3** - a sealed knock row an owner or admin opens with the
 *   group's knock seed (`ChatClient.applyKnocks`).
 *
 * Each hands the opened envelope to {@link deliverReentryRequest}; a request
 * that arrives before the runtime installed its handler is held (bounded).
 *
 * ## The decision
 *
 * The core judges the KeyPackage (`Group.reentryRequestTarget`: a valid
 * package with a marker for this group and this device's current epoch, a
 * current leaf with its signature key and account) and the rest
 * (`reentryAdmissionVerdict`: the device is still the account's, the leaf did
 * not write at the current epoch within the hour, not handled at this epoch,
 * not accepted within the hour). This module only gathers the inputs and
 * decides who may act per route:
 *
 * | Route | Who acts |
 * |---|---|
 * | own-device | a device of the same account in the conversation |
 * | mailbox | the other person of a one-to-one |
 * | knock | the owner or an admin |
 *
 * On `Admit` it proposes `Add(K)` then `Remove(L)` through the ADR-0045 path
 * (the sweep commits them through the ADR-0081 commit route; the Welcome goes
 * to the person's account mailbox, C-69), and records that the gap's
 * messages are owed once the replacement lands (section 7,
 * `reentry-history.ts`).
 *
 * Log lines carry no identifier.
 */

import { base64Encode } from "../auth/account-id";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";

/** A request that arrived by one of the three routes. */
export interface ReentryArrival {
  readonly route: "own-device" | "mailbox" | "knock";
  readonly groupId: Uint8Array;
  readonly epochLeft: bigint;
  readonly gapFromMs: bigint;
  readonly keyPackage: Uint8Array;
  /** Route 1: the leaf keys (lower-case hex) of this account's DeviceGroup
   *  roster as the row that carried it was read. */
  readonly deviceGroupKeysHex?: ReadonlySet<string>;
}

/** What `Group.reentryRequestTarget` returned, read out. */
export interface ReentryTargetFacts {
  readonly accountKey: Uint8Array;
  readonly signatureKey: Uint8Array;
  readonly replacedLeaf: number;
}

/** What the decision needs from the app. */
export interface ReentryAdmitPorts {
  now(): number;
  /** The conversation identity (`chats.identity()`). */
  identity(): WasmIdentity | null;
  /** This account's key inside conversations (standard base64). */
  ownPersonKeyB64(): string | null;
  group(groupIdB64: string): WasmGroup | undefined;
  /** This device's role in `group`; `null` when it has none or no table. */
  ownRole(group: WasmGroup): "owner" | "admin" | "member" | null;
  /** The conversation kind (ADR-0062). */
  kindOf(group: WasmGroup): "chat" | "group";
  /** Routes 2 and 3: the account's directory lists a device with that key. */
  directoryListsDevice(accountKey: Uint8Array, deviceKey: Uint8Array): Promise<boolean>;
  /** The core's `reentryAdmissionVerdict`; `0` is `Admit`. */
  admissionVerdict(input: {
    readonly stillTheAccounts: boolean;
    readonly lastWriteMs: number | undefined;
    readonly handledAtThisEpoch: boolean;
    readonly lastAcceptedMs: number | undefined;
    readonly nowMs: number;
  }): number;
  /** Propose the pair and get it swept. */
  propose(group: WasmGroup, keyPackage: Uint8Array): Promise<void>;
  /** The replacement's gap messages are owed once the group is at
   *  `epochAfter`. */
  noteGapOwed(input: {
    readonly groupIdB64: string;
    /** The returning leaf's signature key (the exporter's context). */
    readonly leafKey: Uint8Array;
    readonly epochAfter: bigint;
    readonly gapFromMs: number;
    readonly route: ReentryArrival["route"];
  }): void;
}

/** Why a request was not acted on (the log line, and the tests). */
export type ReentryAdmitOutcome =
  | "admitted"
  | "no-group"
  | "other-epoch"
  | "not-a-reentry"
  | "not-ours-to-act"
  | "refused"
  | "failed";

/** One leaf's last application message at the current epoch. */
interface LeafWrite {
  readonly epoch: bigint;
  readonly atMs: number;
}

/** The last application messages decrypted per (group, leaf key). Memory
 *  only: an hour-long window does not need to survive a restart. */
const leafWrites = new Map<string, LeafWrite>();

/** Bound on the map (chosen, not measured): a device sees a few hundred
 *  writers at most. */
const MAX_LEAF_WRITES = 4096;

/** An application message from `leafKeyB64` decrypted at `epoch`. */
export function noteLeafWrote(groupIdB64: string, leafKeyB64: string, epoch: bigint, atMs: number): void {
  const key = `${groupIdB64}:${leafKeyB64}`;
  leafWrites.delete(key);
  leafWrites.set(key, { epoch, atMs });
  if (leafWrites.size > MAX_LEAF_WRITES) {
    const oldest = leafWrites.keys().next().value;
    if (oldest !== undefined) leafWrites.delete(oldest);
  }
}

/** The last write of a leaf at `epoch`, unix ms. */
export function lastLeafWriteAt(groupIdB64: string, leafKeyB64: string, epoch: bigint): number | undefined {
  const w = leafWrites.get(`${groupIdB64}:${leafKeyB64}`);
  return w !== undefined && w.epoch === epoch ? w.atMs : undefined;
}

/** What this device handled per leaf. Memory only (section 4's bounds are an
 *  hour; a restart allows at most one more acceptance, and the DS's one
 *  commit per epoch still leaves one replacement). */
const handled = new Map<string, { epoch: bigint; lastAcceptedMs: number }>();

/** Tests. */
export function resetReentryAdmitMemory(): void {
  leafWrites.clear();
  handled.clear();
}

function hexOf(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= (a[i] ?? 0) ^ (b[i] ?? 0);
  return diff === 0;
}

function readTarget(group: WasmGroup, identity: WasmIdentity, keyPackage: Uint8Array): ReentryTargetFacts | null {
  let target;
  try {
    target = group.reentryRequestTarget(identity, keyPackage);
  } catch {
    return null;
  }
  try {
    return {
      accountKey: target.accountKey,
      signatureKey: target.signatureKey,
      replacedLeaf: target.replacedLeaf,
    };
  } finally {
    target.free();
  }
}

function readEpoch(group: WasmGroup): bigint | undefined {
  try {
    return group.epoch;
  } catch {
    return undefined;
  }
}

/** Judge one request and act on it. Never throws. */
export async function handleReentryRequest(arrival: ReentryArrival, ports: ReentryAdmitPorts): Promise<ReentryAdmitOutcome> {
  const outcome = await judgeAndAct(arrival, ports);
  // eslint-disable-next-line no-console
  console.info(`chat re-entry request: route=${arrival.route} outcome=${outcome}`);
  return outcome;
}

async function judgeAndAct(arrival: ReentryArrival, ports: ReentryAdmitPorts): Promise<ReentryAdmitOutcome> {
  const groupIdB64 = base64Encode(arrival.groupId);
  const group = ports.group(groupIdB64);
  const identity = ports.identity();
  if (group === undefined || identity === null) return "no-group";
  const epoch = readEpoch(group);
  if (epoch === undefined || epoch !== arrival.epochLeft) return "other-epoch";
  const target = readTarget(group, identity, arrival.keyPackage);
  if (target === null) return "not-a-reentry";
  const own = ports.ownPersonKeyB64();
  const ownsRequester = own !== null && base64Encode(target.accountKey) === own;
  // Who may act, per route (section 4's table).
  switch (arrival.route) {
    case "own-device":
      if (!ownsRequester) return "not-ours-to-act";
      break;
    case "mailbox":
      if (ownsRequester || ports.kindOf(group) !== "chat") return "not-ours-to-act";
      break;
    case "knock": {
      const role = ports.ownRole(group);
      if (role !== "owner" && role !== "admin") return "not-ours-to-act";
      break;
    }
  }
  // Still the account's device: route 1 by the DeviceGroup roster, routes 2
  // and 3 by the directory (a removed device is deleted and tombstoned).
  let stillTheAccounts: boolean;
  if (arrival.route === "own-device") {
    stillTheAccounts = arrival.deviceGroupKeysHex?.has(hexOf(target.signatureKey)) === true;
  } else {
    try {
      stillTheAccounts = await ports.directoryListsDevice(target.accountKey, target.signatureKey);
    } catch {
      stillTheAccounts = false;
    }
  }
  const leafKeyB64 = base64Encode(target.signatureKey);
  const handledKey = `${groupIdB64}:${leafKeyB64}`;
  const prior = handled.get(handledKey);
  const nowMs = ports.now();
  const verdict = ports.admissionVerdict({
    stillTheAccounts,
    lastWriteMs: lastLeafWriteAt(groupIdB64, leafKeyB64, epoch),
    handledAtThisEpoch: prior !== undefined && prior.epoch === epoch,
    lastAcceptedMs: prior?.lastAcceptedMs,
    nowMs,
  });
  if (verdict !== 0) return "refused";
  // The directory read was a network round trip: the group may have moved.
  if (readEpoch(group) !== epoch) return "other-epoch";
  // Re-judged against the group as it is now; nothing else changed it.
  const again = readTarget(group, identity, arrival.keyPackage);
  if (again === null || !bytesEqual(again.signatureKey, target.signatureKey)) return "not-a-reentry";
  handled.set(handledKey, { epoch, lastAcceptedMs: nowMs });
  try {
    await ports.propose(group, arrival.keyPackage);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn(`chat re-entry: replacement not proposed: ${err instanceof Error ? err.name : "error"}`);
    return "failed";
  }
  const gapFromMs = Number(arrival.gapFromMs);
  ports.noteGapOwed({
    groupIdB64,
    leafKey: target.signatureKey,
    epochAfter: epoch + 1n,
    gapFromMs: Number.isFinite(gapFromMs) ? gapFromMs : nowMs,
    route: arrival.route,
  });
  return "admitted";
}

// ---------- intake ---------------------------------------------------------

/** The handler the runtime installs. */
/**
 * Whether the re-admitter re-sends the push routing secret once the
 * replacement landed: only as the owner or an admin. The ADR-0042 receive
 * gate keeps a plain member's secret only when it is the bytes already held,
 * so a plain member's re-send is refused there and only logs a refusal on the
 * returning device (live walk 2026-11-19). A device brought back by a plain
 * member gets the secret at the owner's next rotation or redistribution
 * (G-58). Same rule on iOS and Android.
 */
export function resendsPushRoutingSecret(ownRole: "owner" | "admin" | "member" | null): boolean {
  return ownRole === "owner" || ownRole === "admin";
}

export type ReentryIntake = (arrival: ReentryArrival) => void;

let intake: ReentryIntake | null = null;
const held: ReentryArrival[] = [];
/** Requests held before the runtime is up (chosen, not measured). */
const MAX_HELD_ARRIVALS = 32;

/** Install (or, with `null`, remove) the handler. Held requests are handed
 *  over at once. */
export function installReentryIntake(handler: ReentryIntake | null): void {
  intake = handler;
  if (handler === null) return;
  for (const a of held.splice(0)) handler(a);
}

/** A request arrived by one of the routes. Never throws. */
export function deliverReentryRequest(arrival: ReentryArrival): void {
  if (intake === null) {
    if (held.length >= MAX_HELD_ARRIVALS) held.shift();
    held.push(arrival);
    return;
  }
  try {
    intake(arrival);
  } catch {
    /* the request is lost; the device re-requests in 30 minutes */
  }
}

/** The arrivals held for a handler (tests). */
export function heldReentryArrivals(): readonly ReentryArrival[] {
  return held;
}
