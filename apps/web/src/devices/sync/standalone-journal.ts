// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The real {@link StandaloneJournal} of `standalone-journal-read.ts`: this
 * device's persisted DeviceGroup, opened into a throwaway handle (never
 * founded, never persisted), read with the DS proof of its current epoch.
 */
import type { DeviceCipher } from "../../auth/device-cipher";
import { fetchDeviceAttestKey } from "../../auth/device-attest-key";
import { getDeviceIdentitySeed } from "../../auth/device-identity-store";
import { DsClient, DsHttpError } from "../../ds/client";
import { takeSigner } from "../../ds/access";
import { loadWasm } from "../../wasm";
import { getDeviceGroupJournalCursor } from "../device-group-snapshot-store";
import { decodeJournalEntry } from "../journal-envelope";
import { DeviceGroupNotJoinedError, ensureDeviceGroup } from "../lifecycle";
import { startingCursor } from "./journal-batch";
import type { StandaloneJournal } from "./standalone-journal-read";

/** Open this device's DeviceGroup for one read, or `null` when the device
 *  holds no usable DeviceGroup state. */
export async function openStandaloneDeviceGroupJournal(input: {
  readonly accountIdB64: string;
  readonly accountIdBytes: Uint8Array;
  readonly deviceKey: DeviceCipher;
  /** The account seed this device holds (the DeviceGroup id derives from
   *  the account's lineage seed). */
  readonly accountIdentitySeed: Uint8Array;
}): Promise<StandaloneJournal | null> {
  const deviceSeed = await getDeviceIdentitySeed(input.accountIdB64, input.deviceKey);
  if (deviceSeed === null) return null;
  let ensured;
  try {
    // No `mayFound`: a device without DeviceGroup state is told so, never
    // given a new DeviceGroup.
    ensured = await ensureDeviceGroup(
      input.accountIdBytes,
      input.accountIdB64,
      deviceSeed,
      input.deviceKey,
      input.accountIdentitySeed,
    );
  } catch (err) {
    if (err instanceof DeviceGroupNotJoinedError) return null;
    throw err;
  } finally {
    deviceSeed.fill(0);
  }
  const { group, identity } = ensured;
  const free = (): void => {
    try {
      group.free();
    } catch {
      /* already freed */
    }
    try {
      identity.free();
    } catch {
      /* already freed */
    }
  };
  let cursor = 0;
  let attestVk: Uint8Array;
  try {
    cursor = startingCursor(await getDeviceGroupJournalCursor(input.accountIdB64));
    // The removal-attestation key (G-13): a removal is applied as the live
    // loop applies it, so the epoch the rows below are sealed at is reached.
    attestVk = await fetchDeviceAttestKey();
  } catch (err) {
    free();
    throw err;
  }
  const wasm = await loadWasm();
  const ds = new DsClient();
  const groupId = group.groupId();
  return {
    cursor,
    async fetch(after) {
      // The access key of the epoch the throwaway handle is at NOW: applied
      // commits move it.
      const signer = takeSigner(() => group.dsAccessSigner(identity));
      try {
        const page = await ds.fetchGroup(groupId, { after, waitSecs: 0, signer });
        return { rows: page.messages.map((m) => ({ id: m.id, payload: m.payload })), hasMore: page.hasMore };
      } finally {
        signer?.free();
      }
    },
    async apply(row) {
      const entry = decodeJournalEntry(row.payload);
      const processed = group.processIncomingVerifyingRemovals(
        identity,
        entry.message,
        entry.attestation ?? new Uint8Array(0),
        attestVk,
        BigInt(Math.floor(Date.now() / 1000)),
      );
      if (processed.kind !== "Application") return null;
      const data = processed.applicationData;
      if (data === undefined) return null;
      const parsed = wasm.parseDeviceGroupPayload(data);
      try {
        return parsed.kind === "serviceEnvelope" ? parsed.serviceEnvelope.slice() : null;
      } finally {
        parsed.free();
      }
    },
    free,
  };
}

/** What a failed standalone fetch means. */
export function classifyStandaloneFetchError(err: unknown): "unreadable" | "offline" {
  return err instanceof DsHttpError && (err.status === 401 || err.status === 403) ? "unreadable" : "offline";
}
