// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0080 amendment (2026-11-23): the device cards a confirming device hands
 * a new device in the pairing state bundle, so the new device's Devices
 * screen names the account's other devices at once instead of waiting for
 * each of them to re-send its card in the foreground.
 *
 * Sending: every card this device holds for a leaf of the DeviceGroup after
 * the add, plus its own. Receiving: each card is decoded with the core codec
 * and kept only for a current leaf of this device's roster other than its own,
 * and only when strictly newer than the card held for that key. The card is
 * attributed by the confirming device, an own device of the account, not by
 * the device it describes: acceptable for display-only data.
 */

import type { Wasm } from "../wasm";
import { base64Decode, base64Encode } from "../auth/account-id";
import { decodeDeviceCardEnvelope } from "./device-card-intake";
import { deviceCardsForStateBundle, storeCarriedDeviceCards } from "./device-card-runtime";
import type { StoredDeviceCard } from "./device-cards";
import type { CarriedDeviceCard, StateBundle } from "./state-bundle";

/** What this code reads of a DeviceGroup handle. */
export interface DeviceGroupRoster {
  devices(): unknown[];
  ownLeafIndex(): number;
}

function hexOf(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

function hexBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** The roster's leaf keys (lower-case hex) and this device's own, or `null`
 *  when the handle cannot be read or does not hold this device. */
export function rosterOf(group: DeviceGroupRoster): { keysHex: string[]; ownHex: string } | null {
  try {
    const own = group.ownLeafIndex();
    const members = group.devices() as { leafIndex: number; signaturePublicKey: Uint8Array }[];
    const keysHex = members.map((m) => hexOf(m.signaturePublicKey));
    const ownMember = members.find((m) => m.leafIndex === own);
    if (ownMember === undefined) return null;
    return { keysHex, ownHex: hexOf(ownMember.signaturePublicKey) };
  } catch {
    return null;
  }
}

/** Confirming device: the `deviceCards` extras of the state bundle. Empty on
 *  any failure (a bundle without cards is a pairing like any other). */
export async function collectStateBundleDeviceCards(
  group: DeviceGroupRoster,
  accountIdB64: string,
  nowMs: number = Date.now(),
): Promise<CarriedDeviceCard[]> {
  const roster = rosterOf(group);
  if (roster === null) return [];
  try {
    const picked = await deviceCardsForStateBundle(accountIdB64, roster.ownHex, roster.keysHex, nowMs);
    return picked.map((c) => ({ leafKeyB64: base64Encode(hexBytes(c.keyHex)), envelopeB64: base64Encode(c.envelope) }));
  } catch {
    return [];
  }
}

/** The carried entries that decode, keyed by lower-case hex. Pure apart from
 *  the codec. */
export function decodeCarriedDeviceCards(
  wasm: Pick<Wasm, "decodeDeviceCardEnvelope">,
  entries: readonly CarriedDeviceCard[],
): { keyHex: string; card: StoredDeviceCard }[] {
  const out: { keyHex: string; card: StoredDeviceCard }[] = [];
  for (const e of entries) {
    let key: Uint8Array;
    let envelope: Uint8Array;
    try {
      key = base64Decode(e.leafKeyB64);
      envelope = base64Decode(e.envelopeB64);
    } catch {
      continue;
    }
    const card = decodeDeviceCardEnvelope(wasm, envelope);
    if (card === null) continue;
    out.push({ keyHex: hexOf(key), card });
  }
  return out;
}

/** New device: store the handed-over cards. Returns how many were kept. */
export function applyStateBundleDeviceCards(
  wasm: Pick<Wasm, "decodeDeviceCardEnvelope">,
  bundle: Pick<StateBundle, "deviceCards">,
  accountIdB64: string,
  group: DeviceGroupRoster,
): number {
  const entries = bundle.deviceCards ?? [];
  if (entries.length === 0) return 0;
  const roster = rosterOf(group);
  if (roster === null) return 0;
  return storeCarriedDeviceCards(accountIdB64, decodeCarriedDeviceCards(wasm, entries), roster.keysHex, roster.ownHex);
}
