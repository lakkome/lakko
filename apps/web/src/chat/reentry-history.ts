// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 phase 2 section 7 - the messages sent while a device was out of a
 * conversation reach it from the device that brought it back.
 *
 * ## The carrier (one for all three request routes; same on iOS and Android)
 *
 * - **secret** = `Group.exportSecret({@link GAP_HISTORY_LABEL}, context =
 *   the returning leaf's 32-byte signature key, 32)`, exported at epoch
 *   `epoch_left + 1` (the epoch the replacement created) by both the
 *   re-admitter and the returning device;
 * - **rendezvous id** = `SHA-256({@link GAP_HISTORY_RENDEZVOUS_DOMAIN} ‖
 *   secret)`, no separator byte;
 * - **payload**: the history bundle sealed under that secret
 *   (`sealHistoryBundleStructured`, the pairing bundle's format) holding the
 *   gap's rows (`createdAt >= gap_from_ms`).
 *
 * Only the members after that commit hold the secret; the returning device
 * holds it the moment it joins. The re-admitter posts once its own group is
 * at exactly that epoch (a later commit changes the exporter; then nothing is
 * sent and the gap line stays).
 *
 * ## When the replacement lands
 *
 * The re-admitter also re-sends the group state a newcomer receives on growth
 * (name, photo, pins, profile, push routing secret, knock key): a
 * replacement changes no member set, so no member sees growth, and the
 * returning device starts from an empty state at the new epoch.
 */

import { base64Encode } from "../auth/account-id";
import type { PersistedMessage } from "./message-log-store";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";

/** The exporter label of the secret. Must match iOS and Android. */
export const GAP_HISTORY_LABEL = "lakko/conversation-reentry/history/v1";
/** The domain hashed in front of the secret for the rendezvous id. Must
 *  match iOS and Android. */
export const GAP_HISTORY_RENDEZVOUS_DOMAIN = "lakko/conversation-reentry/history-rendezvous/v1";

/** Told when the gap's messages of a conversation were filed into the log,
 *  so an open chat shows them at once. */
export type GapHistoryListener = (groupIdB64: string, rows: readonly PersistedMessage[]) => void;

const listeners = new Set<GapHistoryListener>();

/** Listen for filed gap messages. Returns the unsubscribe. */
export function onGapHistoryFiled(listener: GapHistoryListener): () => void {
  listeners.add(listener);
  return () => void listeners.delete(listener);
}

/** The gap's messages of `groupIdB64` were filed. Never throws. */
export function emitGapHistoryFiled(groupIdB64: string, rows: readonly PersistedMessage[]): void {
  for (const l of [...listeners]) {
    try {
      l(groupIdB64, rows);
    } catch {
      /* a listener never fails the filing */
    }
  }
}

/** An owed send is forgotten after a day (chosen, not measured). */
export const GAP_OWED_TTL_MS = 24 * 60 * 60_000;

/** The key and the rendezvous id. */
export interface GapChannel {
  readonly secret: Uint8Array;
  readonly rendezvous: Uint8Array;
}

/** `SHA-256(domain ‖ secret)`. */
export async function gapRendezvousOf(secret: Uint8Array): Promise<Uint8Array> {
  const domain = new TextEncoder().encode(GAP_HISTORY_RENDEZVOUS_DOMAIN);
  const input = new Uint8Array(domain.length + secret.length);
  input.set(domain, 0);
  input.set(secret, domain.length);
  return new Uint8Array(await globalThis.crypto.subtle.digest("SHA-256", input));
}

/** The gap channel of `group` at its current epoch, for the leaf whose
 *  signature key is `returningLeafKey`. */
export async function gapChannelOf(group: WasmGroup, identity: WasmIdentity, returningLeafKey: Uint8Array): Promise<GapChannel> {
  const secret = group.exportSecret(identity, GAP_HISTORY_LABEL, returningLeafKey, 32);
  return { secret, rendezvous: await gapRendezvousOf(secret) };
}

/** One owed send of the re-admitter. */
interface Owed {
  readonly groupIdB64: string;
  readonly leafKey: Uint8Array;
  readonly epochAfter: bigint;
  readonly gapFromMs: number;
  readonly notedAtMs: number;
}

/** Keyed by `group:leaf`. */
const owed = new Map<string, Owed>();

/** Tests. */
export function resetGapHistoryMemory(): void {
  owed.clear();
}

/** The re-admitter proposed a replacement; send once it lands. */
export function noteGapHistoryOwed(input: {
  readonly groupIdB64: string;
  readonly leafKey: Uint8Array;
  readonly epochAfter: bigint;
  readonly gapFromMs: number;
  readonly nowMs: number;
}): void {
  owed.set(`${input.groupIdB64}:${base64Encode(input.leafKey)}`, {
    groupIdB64: input.groupIdB64,
    leafKey: input.leafKey,
    epochAfter: input.epochAfter,
    gapFromMs: input.gapFromMs,
    notedAtMs: input.nowMs,
  });
}

/** Whether a send is owed for the group. */
export function gapHistoryOwed(groupIdB64: string): boolean {
  for (const o of owed.values()) if (o.groupIdB64 === groupIdB64) return true;
  return false;
}

/** What the send needs from the app. */
export interface GapSendPorts {
  now(): number;
  identity(): WasmIdentity | null;
  /** The replacement landed: re-send the growth-time group state. */
  landed(group: WasmGroup): void;
  /** Build the sealed bundle from this device's log (`null`: nothing held). */
  build(group: WasmGroup, secret: Uint8Array, fromMs: number): Promise<Uint8Array | null>;
  publish(rendezvous: Uint8Array, sealed: Uint8Array): Promise<void>;
}

/**
 * A poll of `group` applied: for each owed send of the group, send the bundle
 * if the group is at the replacement's epoch, drop it if the group moved past
 * or it is stale. Never throws. Returns what happened to each owed send.
 */
export async function afterGroupPollForGapHistory(group: WasmGroup, ports: GapSendPorts): Promise<("sent" | "waiting" | "dropped")[]> {
  let groupIdB64: string;
  let epoch: bigint;
  try {
    groupIdB64 = base64Encode(group.groupId);
    epoch = group.epoch;
  } catch {
    return [];
  }
  const out: ("sent" | "waiting" | "dropped")[] = [];
  let landedSignalled = false;
  for (const [key, entry] of [...owed]) {
    if (entry.groupIdB64 !== groupIdB64) continue;
    if (ports.now() - entry.notedAtMs > GAP_OWED_TTL_MS || epoch > entry.epochAfter) {
      owed.delete(key);
      out.push("dropped");
      continue;
    }
    if (epoch < entry.epochAfter) {
      out.push("waiting");
      continue;
    }
    owed.delete(key);
    if (!landedSignalled) {
      landedSignalled = true;
      try {
        ports.landed(group);
      } catch {
        /* the growth re-sends are best-effort */
      }
    }
    const identity = ports.identity();
    if (identity === null) {
      out.push("dropped");
      continue;
    }
    try {
      const channel = await gapChannelOf(group, identity, entry.leafKey);
      const sealed = await ports.build(group, channel.secret, entry.gapFromMs);
      if (sealed === null) {
        out.push("dropped");
        continue;
      }
      await ports.publish(channel.rendezvous, sealed);
      // eslint-disable-next-line no-console
      console.info("chat re-entry: missed messages sent");
      out.push("sent");
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(`chat re-entry: missed messages not sent: ${err instanceof Error ? err.name : "error"}`);
      out.push("dropped");
    }
  }
  return out;
}
