// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Attested-removal policy enablement (G-13, ADR-0015 sprint 5).
 *
 * Once an account has a passkey, device removals should require a
 * peer-verified attestation. This flips the DeviceGroup `require attested
 * removals` policy flag ON via a GroupContextExtensions commit, broadcast over
 * the DeviceGroup journal so every device enforces it on subsequent removals.
 *
 * Idempotent: a no-op when the policy is already on. Called after a successful
 * passkey registration (`PasskeysSection`).
 */
import {
  ensureDeviceGroup,
  persistDeviceGroupSnapshot,
  registerDeviceGroupEpochOrThrow,
} from "./lifecycle";
import { takeSigner } from "../ds/access";
import { encodeJournalEntry } from "./journal-envelope";
import { DsClient } from "../ds/client";

/**
 * Enable attested removals on the local DeviceGroup if not already on.
 *
 * @throws if the DeviceGroup cannot be ensured or the commit cannot be
 *   published — callers treat a throw as non-fatal (the policy can be enabled
 *   on a later passkey action).
 */
export async function enableAttestedRemovalsIfNeeded(args: {
  readonly accountIdBytes: Uint8Array;
  readonly accountIdB64: string;
  readonly deviceKey: CryptoKey;
}): Promise<void> {
  const { ensureDeviceIdentitySeed } = await import("../auth/device-identity-store");
  const seed = await ensureDeviceIdentitySeed(args.accountIdB64, args.deviceKey);
  const ensured = await ensureDeviceGroup(
    args.accountIdBytes,
    args.accountIdB64,
    seed,
    args.deviceKey,
  );
  seed.fill(0);

  try {
    if (ensured.group.requireAttestedRemovals()) {
      return; // already on — nothing to do.
    }
    // ADR-0053 — the key of the epoch the policy commit leaves.
    const signerN = takeSigner(() => ensured.group.dsAccessSigner(ensured.identity));
    try {
      const commit = ensured.group.setRequireAttestedRemovals(ensured.identity);
      const groupId = ensured.group.groupId();
      const ds = new DsClient();
      // ADR-0053 contract point 3 — register before the publish.
      await registerDeviceGroupEpochOrThrow(ds, ensured.group, ensured.identity, signerN);
      // The policy commit removes nothing, so it carries no attestation.
      await ds.publishGroup(groupId, encodeJournalEntry(commit, null), { signer: signerN });
    } finally {
      signerN?.free();
    }
    await persistDeviceGroupSnapshot(ensured.identity, args.accountIdB64, args.deviceKey);
  } finally {
    ensured.group.free();
    ensured.identity.free();
  }
}
