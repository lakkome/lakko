// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0080: a DeviceGroup `serviceEnvelope` that is a device card, taken in.
 *
 * The card names nothing about its sender. The sender is the
 * MLS-authenticated leaf the DeviceGroup reported for this row, resolved to
 * its signature key against the roster AT RECEIPT (C-68: a leaf index is
 * never stored, since OpenMLS hands a vacated leaf to the next device). A
 * row with no sender leaf, or a leaf the roster does not hold, is dropped.
 */

import type { Wasm } from "../wasm";
import { isDevicePlatform, type StoredDeviceCard } from "./device-cards";
import { receiveOwnDeviceCard } from "./device-card-runtime";

type WasmDeviceCard = InstanceType<Wasm["DeviceCard"]>;

/** One DeviceGroup member as the sync runtime reads it. */
export interface RosterMember {
  readonly leaf: number;
  readonly key: string;
}

/**
 * The card inside `envelope`, as this client stores it, or `null` when the
 * envelope is not a well-formed device card.
 */
export function decodeDeviceCardEnvelope(
  wasm: Pick<Wasm, "decodeDeviceCardEnvelope">,
  envelope: Uint8Array,
): StoredDeviceCard | null {
  let card: WasmDeviceCard;
  try {
    card = wasm.decodeDeviceCardEnvelope(envelope);
  } catch {
    return null;
  }
  try {
    const platform = card.platform;
    if (!isDevicePlatform(platform)) return null;
    return {
      platform,
      model: card.model,
      appVersion: card.appVersion,
      os: card.os,
      city: card.city,
      country: card.country,
      updatedAtMs: Number(card.updatedAtMs),
      lastActiveMs: Number(card.lastActiveMs),
    };
  } finally {
    card.free();
  }
}

/** The signature key (hex) the roster holds at `senderLeaf`, or `null`. */
export function senderKeyAtReceipt(
  senderLeaf: number | undefined,
  roster: readonly RosterMember[] | null,
): string | null {
  if (senderLeaf === undefined || roster === null) return null;
  return roster.find((m) => m.leaf === senderLeaf)?.key ?? null;
}

/**
 * Take in `envelope` when it is a device card. Returns `true` when it was one
 * (kept or dropped), `false` for any other envelope, which the caller hands
 * on to the other service-envelope routes.
 */
export function receiveDeviceCardEnvelope(
  wasm: Pick<Wasm, "decodeDeviceCardEnvelope">,
  envelope: Uint8Array,
  senderLeaf: number | undefined,
  roster: readonly RosterMember[] | null,
): boolean {
  const card = decodeDeviceCardEnvelope(wasm, envelope);
  if (card === null) return false;
  const key = senderKeyAtReceipt(senderLeaf, roster);
  if (key === null || roster === null) return true;
  receiveOwnDeviceCard(
    key,
    card,
    roster.map((m) => m.key),
  );
  return true;
}
