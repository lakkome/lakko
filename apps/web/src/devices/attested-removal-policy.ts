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
import type { DeviceCipher } from "../auth/device-cipher";
import { ensureDeviceGroup } from "./lifecycle";
import { encodeJournalEntry } from "./journal-envelope";
import { commitDeviceGroupChange } from "./device-group-commit-runtime";

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
  readonly deviceKey: DeviceCipher;
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
  } finally {
    ensured.group.free();
    ensured.identity.free();
  }
  // ADR-0081 decision 2 — through the DeviceGroup commit outbox. The policy
  // commit removes nothing, so its row carries no attestation. A lost epoch
  // leaves the policy off; the next passkey action enables it.
  await commitDeviceGroupChange(
    { accountIdBytes: args.accountIdBytes, accountIdB64: args.accountIdB64, deviceKey: args.deviceKey },
    (fresh) =>
      fresh.group.requireAttestedRemovals()
        ? null
        : { kind: "policy", row: encodeJournalEntry(fresh.group.setRequireAttestedRemovals(fresh.identity), null) },
    async () => undefined,
  );
}
