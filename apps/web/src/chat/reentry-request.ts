// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 phase 2 section 3 - a device behind in a conversation asks to be
 * brought back.
 *
 * As soon as the device is suspected, 2 minutes after a request that went to
 * its own other device, and otherwise no more often than every 30 minutes
 * (`reentryRequestDue`, the core's rule), it mints a re-entry
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
 * Route 1 is tried once per behind episode: an own device with a leaf in the
 * conversation may not be running (live walk 5, 2026-11-20), so once a
 * request went to it (`ownDeviceAsked` on the record) the core's
 * `reentryRoute` sends every later request of the episode by route 2 or 3.
 * The same holds when route 1 cannot send at all: no DeviceGroup sync on
 * this device, or a send to it that failed, makes the own device unavailable
 * for the episode, so the device is never stuck on route 1.
 *
 * The request carries the time of the first row this device could not read
 * (`gap_from_ms`) and a fresh public gap key (`generateGapKey`), which the
 * device that brings it back uses to send the missed messages it holds
 * (section 7). The gap secret is sealed under the device key and recorded
 * with the request before anything is sent.
 *
 * A failed send is retried after {@link SEND_RETRY_MS}; the record keeps the
 * KeyPackage, so a Welcome that answers it can still be joined. Log lines
 * carry no identifier.
 */

import { base64Decode, base64Encode } from "../auth/account-id";
import type { BehindStateView } from "./conversation-behind";
import {
  markOwnDeviceUnavailable,
  markReentryRequestSent,
  recordReentryRequest,
  reentryRecordFor,
  type HeldKnockKey,
  type ReentryRouteName,
  type SealedGapSecret,
} from "./reentry-store";
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
  due(state: BehindStateView, lastSentMs: number | undefined, lastRouteWasOwnDevice: boolean, nowMs: number): boolean;
  /** The core's `reentryRoute`. */
  route(ownDeviceInConversation: boolean, kind: "chat" | "group", ownDeviceUnanswered: boolean): ReentryRouteName;
  /**
   * Whether this device can reach its own other devices now (the account's
   * DeviceGroup sync runs here). When it cannot, the own device is treated as
   * unavailable for the episode, so the request goes by the conversation's
   * own route at once instead of waiting on a route that cannot send.
   */
  ownDeviceReachable(): boolean;
  /** The group's knock key as this device holds it. */
  knockKey(group: WasmGroup): Promise<HeldKnockKey | null>;
  /** Make the MLS snapshot durable (throws when it could not be). */
  persistDurable(): Promise<void>;
  /** A fresh gap key pair (`generateGapKey`). */
  generateGapKey(): { readonly secret: Uint8Array; readonly publicKey: Uint8Array };
  /** Seal a gap secret under the account's device key (throws when it
   *  could not be: the request must not go out). */
  sealGapSecret(secret: Uint8Array): Promise<SealedGapSecret>;
  composeEnvelope(groupId: Uint8Array, epochLeft: bigint, gapFromMs: bigint, gapKey: Uint8Array, keyPackage: Uint8Array): Uint8Array;
  sealKnockRow(knockPk: Uint8Array, knockVersion: number, envelope: Uint8Array): Uint8Array;
  sendOwnDevice(envelope: Uint8Array): Promise<void>;
  sendMailbox(otherAccountKey: Uint8Array, envelope: Uint8Array, groupId: Uint8Array): Promise<void>;
  sendKnock(groupId: Uint8Array, row: Uint8Array): Promise<void>;
  /**
   * Live walk 3 (2026-11-20) - the journal's epoch as the DS names it now:
   * an empty knock (one `ds-knock` token). `null` when the probe failed or
   * named no epoch. Asked right before every request; a stored epoch from an
   * earlier read is never used (it named epoch 1 to an owner at 67).
   */
  probeEpoch(groupId: Uint8Array): Promise<bigint | null>;
}

/** What one attempt came to. */
export type ReentryRequestOutcome =
  | "sent"
  | "not-due"
  | "retry-wait"
  | "no-route"
  | "no-epoch"
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
 * `gapFromMs` where the gap starts. The epoch the request names is probed
 * right before it ({@link ReentryRequestPorts.probeEpoch}). Never throws.
 */
export async function requestReentryIfDue(
  group: WasmGroup,
  input: { readonly state: BehindStateView; readonly gapFromMs: number },
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
  input: { readonly state: BehindStateView; readonly gapFromMs: number },
  ports: ReentryRequestPorts,
): Promise<ReentryRequestOutcome> {
  if (!input.state.stopsActing) return "not-due";
  const groupId = group.groupId;
  const groupIdB64 = base64Encode(groupId);
  const nowMs = ports.now();
  const record = reentryRecordFor(groupIdB64);
  // A record written before a send that did not happen has `lastSentMs` 0.
  const lastSent = record === undefined || record.lastSentMs <= 0 ? undefined : record.lastSentMs;
  const lastRouteWasOwnDevice = lastSent !== undefined && record?.route === "own-device";
  if (!ports.due(input.state, lastSent, lastRouteWasOwnDevice, nowMs)) return "not-due";
  const failedAt = lastFailure.get(groupIdB64);
  if (failedAt !== undefined && nowMs - failedAt < SEND_RETRY_MS) return "retry-wait";

  const identity = ports.identity();
  const own = ports.ownPersonKeyB64();
  const roster = ports.roster(group);
  if (identity === null || own === null || roster === null) return "no-route";
  const kind = ports.kindOf(group);
  const route = ports.route(
    ownDeviceInConversation(roster, own, base64Encode(identity.signaturePublicKey)) && ports.ownDeviceReachable(),
    kind,
    record?.ownDeviceAsked === true,
  );

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

  // The epoch the journal is at now, read right before the request: the
  // marker must name the epoch the re-admitter holds (section 4).
  const epochLeft = await ports.probeEpoch(groupId);
  if (epochLeft === null) {
    lastFailure.set(groupIdB64, nowMs);
    return "no-epoch";
  }

  // Mint, make durable, record - all before anything is sent. The gap
  // secret is sealed first: a request whose secret is not kept would bring
  // back a device that can never open its gap's messages.
  const gapKey = ports.generateGapKey();
  let sealedGapKey: SealedGapSecret;
  try {
    sealedGapKey = await ports.sealGapSecret(gapKey.secret);
  } finally {
    gapKey.secret.fill(0);
  }
  const keyPackage = identity.createReentryKeyPackage(groupId, epochLeft);
  await ports.persistDurable();
  const gapFromMs = record?.gapFromMs ?? input.gapFromMs;
  const dropped = recordReentryRequest({
    groupIdB64,
    keyPackageB64: base64Encode(keyPackage),
    gapKey: sealedGapKey,
    epochLeft,
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

  const envelope = ports.composeEnvelope(
    groupId,
    epochLeft,
    BigInt(Math.max(0, Math.floor(gapFromMs))),
    gapKey.publicKey,
    keyPackage,
  );
  if (route === "own-device") {
    try {
      await ports.sendOwnDevice(envelope);
    } catch (err) {
      // The own device cannot be reached by this device: unavailable for the
      // episode, so the retry goes by the conversation's own route.
      markOwnDeviceUnavailable(groupIdB64);
      throw err;
    }
  } else if (route === "mailbox" && mailboxTo !== null) {
    await ports.sendMailbox(mailboxTo, envelope, groupId);
  } else if (knock !== null) {
    await ports.sendKnock(groupId, ports.sealKnockRow(base64Decode(knock.publicKeyB64), knock.version, envelope));
  }
  // Sent: the clock of the next request starts now.
  markReentryRequestSent(groupIdB64, nowMs);
  lastFailure.delete(groupIdB64);
  // eslint-disable-next-line no-console
  console.info(`chat re-entry request sent: route=${route}`);
  return "sent";
}
