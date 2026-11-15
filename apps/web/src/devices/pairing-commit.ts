// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The DeviceGroup commit a confirming device builds for a pairing payload
 * (`runScanFlow`), on the throwaway handle the commit outbox loads at the
 * durable epoch.
 *
 *   - No self-removal proof: an Add of the payload's KeyPackage, as always.
 *   - G-56 (ADR-0074 "Joining after a rotation"): a device already in the
 *     account's DeviceGroup (it joined by pairing, then parked for the
 *     account's earlier keys) pairs again under the same device identity.
 *     An Add would carry a leaf key the group already has, which the core
 *     refuses (`DuplicateSignatureKey`), so the payload carries the device's
 *     self-removal proof (ADR-0081 amendment) and the confirming device
 *     replaces the old leaf with the new KeyPackage in ONE commit, the proof
 *     in the journal envelope's removal-authorisation slot, as the ADR-0081
 *     rejoin admission does (`sync/DeviceGroupSyncContext.tsx`).
 *
 * A proof that does not cover the replacement at the epoch the commit leaves
 * (a stale epoch, another group, another KeyPackage) is refused before
 * anything is built: nothing is committed and the person tries again.
 */
import type {
  DeviceGroup as WasmDeviceGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";
import { t } from "../l10n";
import { encodeJournalEntry } from "./journal-envelope";
import type { BuiltDeviceGroupCommit } from "./device-group-commit-runtime";
import type { DecodedPairingQr } from "./pairing";

/** The subset of the wasm core the builder needs (the real module in use). */
export interface PairingCommitCore {
  keyPackageSignatureKey(keyPackage: Uint8Array): Uint8Array;
  deviceGroupSelfRemovalCovers(
    identity: WasmIdentity,
    proof: Uint8Array,
    groupId: Uint8Array,
    epoch: bigint,
    removedSignaturePubkey: Uint8Array,
    keyPackage: Uint8Array,
  ): boolean;
}

/** G-56 — the payload's self-removal proof does not cover replacing the
 *  device's leaf at the DeviceGroup's current epoch. Nothing was committed;
 *  the message is the ordinary pairing failure copy. */
export class SelfRemovalProofNotCoveringError extends Error {
  override name = "SelfRemovalProofNotCoveringError";
  constructor() {
    super(t("devices.add.error.generic"));
  }
}

function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) if (a[i] !== b[i]) return false;
  return true;
}

/**
 * The commit for `decoded` on `ensured` (a handle at the epoch the commit
 * leaves). Throws {@link SelfRemovalProofNotCoveringError} when the payload
 * carries a proof, the KeyPackage's key has a leaf in the DeviceGroup, and
 * the proof does not cover replacing it there; nothing is built then.
 */
export function buildPairingCommit(
  core: PairingCommitCore,
  ensured: { readonly identity: WasmIdentity; readonly group: WasmDeviceGroup },
  decoded: Pick<DecodedPairingQr, "keyPackage" | "rendezvousPseudonym" | "selfRemovalProof">,
): BuiltDeviceGroupCommit {
  // ADR-0058 section 5.1 — who was a member before this change, for the
  // growth re-send the sync runtime runs once it is current.
  let members: { leafIndex: number; signaturePublicKey: Uint8Array }[] = [];
  try {
    members = ensured.group.devices() as { leafIndex: number; signaturePublicKey: Uint8Array }[];
  } catch {
    members = [];
  }
  const priorLeaves = members.map((m) => m.leafIndex).sort((a, b) => a - b);
  const proof = decoded.selfRemovalProof;
  if (proof !== null && proof.length > 0) {
    const key = core.keyPackageSignatureKey(decoded.keyPackage);
    const previous = members.find((m) => sameBytes(m.signaturePublicKey, key));
    if (previous !== undefined) {
      const covers = core.deviceGroupSelfRemovalCovers(
        ensured.identity,
        proof,
        ensured.group.groupId(),
        ensured.group.epoch(),
        key,
        decoded.keyPackage,
      );
      if (!covers) {
        // eslint-disable-next-line no-console
        console.warn("pairing: the payload's self-removal proof does not cover this epoch; refused");
        throw new SelfRemovalProofNotCoveringError();
      }
      const replaced = ensured.group.replaceDeviceAt(ensured.identity, previous.leafIndex, decoded.keyPackage);
      // eslint-disable-next-line no-console
      console.info("pairing: a device of this account pairs again; its leaf is replaced in one commit");
      return {
        kind: "rejoin-replace",
        row: encodeJournalEntry(replaced.commit, proof),
        welcome: { bytes: replaced.welcome, rendezvous: decoded.rendezvousPseudonym },
        priorLeaves,
      };
    }
  }
  const addition = ensured.group.addDeviceFromWire(ensured.identity, decoded.keyPackage);
  // G-13 (ADR-0015): every DeviceGroup journal entry is wrapped; an
  // add-device commit carries no attestation.
  return {
    kind: "add",
    row: encodeJournalEntry(addition.commit, null),
    welcome: { bytes: addition.welcome, rendezvous: decoded.rendezvousPseudonym },
    priorLeaves,
  };
}
