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

import { base64Decode, base64Encode } from "../auth/account-id";
import type { PersistedMessage } from "./message-log-store";
import {
  deleteGapLook,
  gapLookFor,
  loadOwedGapSends,
  saveGapLook,
  saveOwedGapSends,
  type StoredGapLook,
  type StoredOwedGapSend,
} from "./reentry-store";
import { DEVICE_CIPHER_IV_LEN, type DeviceCipher } from "../auth/device-cipher";
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

/** An owed send is forgotten 30 days after it was noted, the DS retention
 *  the returning device keeps looking for (chosen, not measured; live walk
 *  2026-11-19: it was a day, in memory). */
export const GAP_OWED_TTL_MS = 30 * 24 * 60 * 60_000;

/** The device that came back looks at most once a minute per group (chosen,
 *  not measured). */
export const GAP_HISTORY_LOOK_INTERVAL_MS = 60_000;

/** It keeps looking for 30 days after it came back, the DS retention; then
 *  the gap line stays (chosen, not measured). */
export const GAP_HISTORY_LOOK_RETENTION_MS = 30 * 24 * 60 * 60_000;

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

function keyOf(o: { readonly groupIdB64: string; readonly leafKeyB64: string }): string {
  return `${o.groupIdB64}:${o.leafKeyB64}`;
}

function toOwed(row: StoredOwedGapSend): Owed | null {
  try {
    return {
      groupIdB64: row.groupIdB64,
      leafKey: base64Decode(row.leafKeyB64),
      epochAfter: BigInt(row.epochAfter),
      gapFromMs: row.gapFromMs,
      notedAtMs: row.notedAtMs,
    };
  } catch {
    return null;
  }
}

/** Tests: nothing is held in memory any more (the owed sends are durable,
 *  `reentry-store.ts`); kept so a test's teardown reads the same. */
export function resetGapHistoryMemory(): void {
  looking.clear();
}

/**
 * The re-admitter proposed a replacement; send once it lands. Durable
 * (live walk 2026-11-19: a hidden re-admitter's tab polled late and the
 * in-memory entry was the only record).
 */
export function noteGapHistoryOwed(input: {
  readonly groupIdB64: string;
  readonly leafKey: Uint8Array;
  readonly epochAfter: bigint;
  readonly gapFromMs: number;
  readonly nowMs: number;
}): void {
  const row: StoredOwedGapSend = {
    groupIdB64: input.groupIdB64,
    leafKeyB64: base64Encode(input.leafKey),
    epochAfter: input.epochAfter.toString(),
    gapFromMs: input.gapFromMs,
    notedAtMs: input.nowMs,
  };
  const all = loadOwedGapSends().filter((o) => keyOf(o) !== keyOf(row));
  if (!saveOwedGapSends([...all, row])) {
    // eslint-disable-next-line no-console
    console.warn("chat re-entry: owed missed messages not recorded");
  }
}

/** Whether a send is owed for the group. */
export function gapHistoryOwed(groupIdB64: string): boolean {
  return loadOwedGapSends().some((o) => o.groupIdB64 === groupIdB64);
}

/** The groups a send is owed for, with the epoch each waits for. */
export function owedGapHistoryGroups(): { readonly groupIdB64: string; readonly epochAfter: bigint }[] {
  const out: { groupIdB64: string; epochAfter: bigint }[] = [];
  for (const row of loadOwedGapSends()) {
    const o = toOwed(row);
    if (o !== null) out.push({ groupIdB64: o.groupIdB64, epochAfter: o.epochAfter });
  }
  return out;
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
  // The group's rows are claimed (removed from the store) before the first
  // wait, so two poll paths never send one bundle twice.
  const all = loadOwedGapSends();
  const mine = all.filter((o) => o.groupIdB64 === groupIdB64);
  if (mine.length === 0) return out;
  const keep = all.filter((o) => o.groupIdB64 !== groupIdB64);
  const due: Owed[] = [];
  for (const row of mine) {
    const entry = toOwed(row);
    if (entry === null || ports.now() - entry.notedAtMs > GAP_OWED_TTL_MS || epoch > entry.epochAfter) {
      out.push("dropped");
      continue;
    }
    if (epoch < entry.epochAfter) {
      keep.push(row);
      out.push("waiting");
      continue;
    }
    due.push(entry);
  }
  saveOwedGapSends(keep);
  for (const entry of due) {
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

// ---------- the device that came back keeps looking (live walk 2026-11-19) --

/**
 * Whether a gap look is due at `nowMs`: dropped once the retention passed
 * since the device came back, held for {@link GAP_HISTORY_LOOK_INTERVAL_MS}
 * after the last look. A clock that moved back counts as due.
 */
export function gapLookStep(
  look: { readonly gapToMs: number; readonly lastLookMs: number },
  nowMs: number,
): "look" | "wait" | "drop" {
  if (nowMs - look.gapToMs > GAP_HISTORY_LOOK_RETENTION_MS) return "drop";
  if (look.lastLookMs > 0 && nowMs >= look.lastLookMs && nowMs - look.lastLookMs < GAP_HISTORY_LOOK_INTERVAL_MS) {
    return "wait";
  }
  return "look";
}

/** What one filed bundle showed (`applyGapHistoryBundle`). */
export interface FiledGapHistory {
  readonly applied: number;
  readonly complete: boolean;
  readonly earliestMs: number | undefined;
  readonly rows: readonly PersistedMessage[];
}

/** What a look needs from the app. */
export interface GapLookPorts {
  now(): number;
  /** The account's device key, or `null`. */
  deviceKey(): Promise<DeviceCipher | null>;
  /** One short read of the rendezvous: every row's payload, or `null` when
   *  the read failed. */
  read(rendezvous: Uint8Array): Promise<readonly Uint8Array[] | null>;
  /** Open and file one row; `null` when it is not the bundle or does not open. */
  apply(secret: Uint8Array, sealed: Uint8Array): Promise<FiledGapHistory | null>;
  /** The bundle was filed: narrow the gap line ending at `gapToMs`, show the rows. */
  received(groupIdB64: string, gapToMs: number, filed: FiledGapHistory): void;
}

/** Groups whose look is running now. */
const looking = new Set<string>();

/**
 * The device came back: keep looking for the gap's messages (durable). The
 * secret is sealed under the device key before it is stored. Returns `false`
 * when nothing could be stored.
 */
export async function recordGapLook(
  input: {
    readonly groupIdB64: string;
    readonly secret: Uint8Array;
    readonly gapFromMs: number;
    readonly gapToMs: number;
  },
  deviceKey: DeviceCipher,
): Promise<boolean> {
  try {
    const iv = globalThis.crypto.getRandomValues(new Uint8Array(DEVICE_CIPHER_IV_LEN));
    const sealed = new Uint8Array(await deviceKey.encrypt(iv, input.secret));
    const look: StoredGapLook = {
      ivB64: base64Encode(iv),
      sealedSecretB64: base64Encode(sealed),
      gapFromMs: input.gapFromMs,
      gapToMs: input.gapToMs,
      lastLookMs: 0,
    };
    return saveGapLook(input.groupIdB64, look);
  } catch {
    return false;
  }
}

/**
 * One look for the gap's messages of `groupIdB64`, if one is due: one short
 * read of the rendezvous; the first row that opens is filed and the look
 * ends. A failed read, no row, or no row that opens keeps the look for the
 * next one. Never throws.
 */
export async function lookForGapHistory(
  groupIdB64: string,
  ports: GapLookPorts,
): Promise<"absent" | "busy" | "wait" | "dropped" | "none" | "received"> {
  if (looking.has(groupIdB64)) return "busy";
  const look = gapLookFor(groupIdB64);
  if (look === undefined) return "absent";
  const nowMs = ports.now();
  const step = gapLookStep(look, nowMs);
  if (step === "drop") {
    deleteGapLook(groupIdB64);
    // eslint-disable-next-line no-console
    console.info("chat re-entry: gap look dropped: retention passed");
    return "dropped";
  }
  if (step === "wait") return "wait";
  looking.add(groupIdB64);
  try {
    // The throttle holds even when this look fails half-way.
    saveGapLook(groupIdB64, { ...look, lastLookMs: nowMs });
    const deviceKey = await ports.deviceKey();
    if (deviceKey === null) return "none";
    let secret: Uint8Array;
    try {
      secret = new Uint8Array(await deviceKey.decrypt(base64Decode(look.ivB64), base64Decode(look.sealedSecretB64)));
    } catch {
      // Sealed under a key this device no longer holds: it never opens.
      deleteGapLook(groupIdB64);
      return "dropped";
    }
    const rows = await ports.read(await gapRendezvousOf(secret)).catch(() => null);
    if (rows === null || rows.length === 0) return "none";
    for (const row of [...rows].reverse()) {
      const filed = await ports.apply(secret, row).catch(() => null);
      if (filed === null) continue;
      deleteGapLook(groupIdB64);
      ports.received(groupIdB64, look.gapToMs, filed);
      // eslint-disable-next-line no-console
      console.info(`chat re-entry: missed messages received: rows=${filed.applied}`);
      return "received";
    }
    return "none";
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn(`chat re-entry: gap look failed: ${err instanceof Error ? err.name : "error"}`);
    return "none";
  } finally {
    looking.delete(groupIdB64);
  }
}

/** Installed by the re-entry runtime: a look for `groupIdB64` now, if due. */
let lookRequester: ((groupIdB64: string) => void) | null = null;

/** Install (or, with `null`, remove) the look requester. */
export function installGapLookRequester(next: ((groupIdB64: string) => void) | null): void {
  lookRequester = next;
}

/** The chat was opened: look for its gap's messages, if a look is due. */
export function requestGapHistoryLook(groupIdB64: string): void {
  try {
    lookRequester?.(groupIdB64);
  } catch {
    /* a look never fails the caller */
  }
}
