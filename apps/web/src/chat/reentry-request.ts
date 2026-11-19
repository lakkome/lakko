// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 phase 2 section 3 - a device behind in a conversation asks to be
 * brought back.
 *
 * As soon as the device is suspected, and then no more often than every 30
 * minutes (`reentryRequestDue`, the core's rule), it mints a re-entry
 * KeyPackage under its own device identity naming the conversation and the
 * epoch the journal is at (`Identity.createReentryKeyPackage`), makes the MLS
 * snapshot that holds its private keys durable, records the request
 * (`reentry-store.ts`) and only then sends the request envelope by the first
 * route that applies (`reentryRoute`):
 *
 * 1. another device of this account is in the conversation: a DeviceGroup
 *    service envelope;
 * 2. a one-to-one: a sealed row in the other person's account mailbox;
 * 3. a group: a sealed knock row to the group's knock key (one `ds-knock`
 *    token), the key the owner sends every member in-group.
 *
 * The request carries the time of the first row this device could not read
 * (`gap_from_ms`), which the device that brings it back uses to send the
 * missed messages it holds (section 7).
 *
 * A failed send is retried after {@link SEND_RETRY_MS}; the record keeps the
 * KeyPackage, so a Welcome that answers it can still be joined. Log lines
 * carry no identifier.
 */

import { base64Decode, base64Encode } from "../auth/account-id";
import type { BehindStateView } from "./conversation-behind";
import { markReentryRequestSent, recordReentryRequest, reentryRecordFor, type HeldKnockKey, type ReentryRouteName } from "./reentry-store";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";

/** A failed send is tried again after this long (chosen, not measured). */
export const SEND_RETRY_MS = 60_000;

/** A roster leaf: its signature key and the person (account) it binds. */
export interface RosterLeaf {
  readonly leafKeyB64: string;
  readonly personKeyB64: string;
}

/** What the request needs from the app. */
export interface ReentryRequestPorts {
  now(): number;
  identity(): WasmIdentity | null;
  /** This account's key inside conversations (standard base64). */
  ownPersonKeyB64(): string | null;
  /** The group's roster as this device last knew it. */
  roster(group: WasmGroup): RosterLeaf[] | null;
  kindOf(group: WasmGroup): "chat" | "group";
  /** `true` while the request is due (the core's `reentryRequestDue`). */
  due(state: BehindStateView, lastSentMs: number | undefined, nowMs: number): boolean;
  /** The core's `reentryRoute`. */
  route(ownDeviceInConversation: boolean, kind: "chat" | "group"): ReentryRouteName;
  /** The group's knock key as this device holds it. */
  knockKey(group: WasmGroup): Promise<HeldKnockKey | null>;
  /** Make the MLS snapshot durable (throws when it could not be). */
  persistDurable(): Promise<void>;
  composeEnvelope(groupId: Uint8Array, epochLeft: bigint, gapFromMs: bigint, keyPackage: Uint8Array): Uint8Array;
  sealKnockRow(knockPk: Uint8Array, knockVersion: number, envelope: Uint8Array): Uint8Array;
  sendOwnDevice(envelope: Uint8Array): Promise<void>;
  sendMailbox(otherAccountKey: Uint8Array, envelope: Uint8Array, groupId: Uint8Array): Promise<void>;
  sendKnock(groupId: Uint8Array, row: Uint8Array): Promise<void>;
}

/** What one attempt came to. */
export type ReentryRequestOutcome =
  | "sent"
  | "not-due"
  | "retry-wait"
  | "no-route"
  | "failed";

/** Failed sends per group, memory only. */
const lastFailure = new Map<string, number>();

/** Tests. */
export function resetReentryRequestMemory(): void {
  lastFailure.clear();
}

/** Whether another device of this account holds a leaf in the roster. */
export function ownDeviceInConversation(roster: readonly RosterLeaf[], ownPersonKeyB64: string, ownLeafKeyB64: string): boolean {
  return roster.some((l) => l.personKeyB64 === ownPersonKeyB64 && l.leafKeyB64 !== ownLeafKeyB64);
}

/** The other person of a one-to-one, or `null`. */
export function otherPerson(roster: readonly RosterLeaf[], ownPersonKeyB64: string): string | null {
  const others = new Set(roster.filter((l) => l.personKeyB64 !== ownPersonKeyB64).map((l) => l.personKeyB64));
  return others.size === 1 ? ([...others][0] ?? null) : null;
}

/**
 * Send the request when it is due. `state` is the group's behind state,
 * `epochLeft` the journal's latest epoch as last read, `gapFromMs` when the
 * device first failed to read a row. Never throws.
 */
export async function requestReentryIfDue(
  group: WasmGroup,
  input: { readonly state: BehindStateView; readonly epochLeft: bigint; readonly gapFromMs: number },
  ports: ReentryRequestPorts,
): Promise<ReentryRequestOutcome> {
  try {
    return await attempt(group, input, ports);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn(`chat re-entry request not sent: ${err instanceof Error ? err.name : "error"}`);
    try {
      lastFailure.set(base64Encode(group.groupId), ports.now());
    } catch {
      /* group gone */
    }
    return "failed";
  }
}

async function attempt(
  group: WasmGroup,
  input: { readonly state: BehindStateView; readonly epochLeft: bigint; readonly gapFromMs: number },
  ports: ReentryRequestPorts,
): Promise<ReentryRequestOutcome> {
  if (!input.state.stopsActing) return "not-due";
  const groupId = group.groupId;
  const groupIdB64 = base64Encode(groupId);
  const nowMs = ports.now();
  const record = reentryRecordFor(groupIdB64);
  // A record written before a send that did not happen has `lastSentMs` 0.
  const lastSent = record === undefined || record.lastSentMs <= 0 ? undefined : record.lastSentMs;
  if (!ports.due(input.state, lastSent, nowMs)) return "not-due";
  const failedAt = lastFailure.get(groupIdB64);
  if (failedAt !== undefined && nowMs - failedAt < SEND_RETRY_MS) return "retry-wait";

  const identity = ports.identity();
  const own = ports.ownPersonKeyB64();
  const roster = ports.roster(group);
  if (identity === null || own === null || roster === null) return "no-route";
  const kind = ports.kindOf(group);
  const route = ports.route(ownDeviceInConversation(roster, own, base64Encode(identity.signaturePublicKey)), kind);

  // Resolve the route's destination BEFORE minting: a request that cannot go
  // anywhere mints nothing.
  let mailboxTo: Uint8Array | null = null;
  let knock: HeldKnockKey | null = null;
  if (route === "mailbox") {
    const other = otherPerson(roster, own);
    if (other === null) return "no-route";
    mailboxTo = base64Decode(other);
  } else if (route === "knock") {
    knock = await ports.knockKey(group);
    if (knock === null) return "no-route";
  }

  // Mint, make durable, record - all before anything is sent.
  const keyPackage = identity.createReentryKeyPackage(groupId, input.epochLeft);
  await ports.persistDurable();
  const gapFromMs = record?.gapFromMs ?? input.gapFromMs;
  const dropped = recordReentryRequest({
    groupIdB64,
    keyPackageB64: base64Encode(keyPackage),
    epochLeft: input.epochLeft,
    gapFromMs,
    sentAtMs: 0,
    route,
  });
  if (dropped.length > 0) {
    for (const old of dropped) {
      try {
        identity.forgetKeyPackage(base64Decode(old));
      } catch {
        /* already gone */
      }
    }
    await ports.persistDurable().catch(() => undefined);
  }

  const envelope = ports.composeEnvelope(groupId, input.epochLeft, BigInt(Math.max(0, Math.floor(gapFromMs))), keyPackage);
  if (route === "own-device") {
    await ports.sendOwnDevice(envelope);
  } else if (route === "mailbox" && mailboxTo !== null) {
    await ports.sendMailbox(mailboxTo, envelope, groupId);
  } else if (knock !== null) {
    await ports.sendKnock(groupId, ports.sealKnockRow(base64Decode(knock.publicKeyB64), knock.version, envelope));
  }
  // Sent: the 30-minute clock starts now.
  markReentryRequestSent(groupIdB64, nowMs);
  lastFailure.delete(groupIdB64);
  // eslint-disable-next-line no-console
  console.info(`chat re-entry request sent: route=${route}`);
  return "sent";
}
