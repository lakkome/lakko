// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * DeviceGroup lifecycle on the web client
 * (Phase 2a3b2-bootstrap, ADR-0007).
 *
 * One entry point — [`ensureDeviceGroup`] — wraps the
 * "load existing or create-founder" decision plus the
 * post-mutation snapshot persistence so the per-route call
 * sites see a single function.
 *
 * Mental model:
 *
 * ```text
 *   signup / login / recover
 *           │
 *           ▼
 *   ensureDeviceGroup(...) ──┬─ snapshot found in IDB ──→ Identity.restoreStorage(snapshot)
 *                            │                            then loadDeviceGroup(identity, accountId)
 *                            │                            → live handle (or null if storage carried
 *                            │                              an unrelated row)
 *                            │
 *                            └─ no snapshot ─────────────→ DeviceGroup.createFounder(identity, accountId)
 *                                                          then identity.snapshotStorage()
 *                                                          then putDeviceGroupSnapshot(...)
 *                                                          → live handle (this device is now the
 *                                                            founding leaf of the user's DeviceGroup)
 * ```
 *
 * Distinct DeviceIdentity provider — the wasm `Identity` this
 * module builds from the cached `deviceIdentitySeed` has its
 * own in-memory OpenMLS provider, independent of the
 * AccountIdentity provider that `ChatProvider` holds for
 * conversation groups (ADR-0007). Snapshot/restore goes
 * through this DeviceIdentity provider's storage only.
 */
import { loadWasm } from "../wasm";
import { base64Decode, base64Encode } from "../auth/account-id";
import { keyResetAttestationGeneration } from "../auth/key-reset-owed";
import { DsClient } from "../ds/client";
import { dsAccess, registerBeforePublish, takeSigner, type ProofSigner } from "../ds/access";
import { claimOrReset, releaseAuthority, resetWithAttestation } from "../ds/ownership";
import type {
  DeviceGroup as WasmDeviceGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";
import {
  getDeviceGroupSnapshot,
  putDeviceGroupSnapshot,
} from "./device-group-snapshot-store";

/** Options of [`ensureDeviceGroup`]. */
export interface EnsureDeviceGroupOptions {
  /**
   * ADR-0065 section 5 - the founding follows a key reset: the journal's root
   * belongs to the OLD seed, so instead of `claimOrReset` (which a stranger
   * cannot win) the founding sends the attested `PUT .../reset` carrying this
   * attestation (STANDARD base64 of the 177 bytes) under which the DS hears
   * the new root. A cached snapshot is NOT restored on this path: it is the
   * old generation's DeviceGroup, and the caller deleted it. Requires
   * `accountIdentitySeed`.
   */
  readonly keyResetAttestB64?: string;
}

/** Result of [`ensureDeviceGroup`]. */
export interface EnsuredDeviceGroup {
  /** The wasm `Identity` built from the DeviceIdentity seed,
   *  populated with the (loaded or freshly-bootstrapped) MLS
   *  provider state. Caller holds this for the duration of
   *  the session; closing the handle wipes the provider. */
  readonly identity: WasmIdentity;
  /** The live DeviceGroup handle. */
  readonly group: WasmDeviceGroup;
  /** `true` when this call bootstrapped a fresh DeviceGroup
   *  (this device is now the founder). `false` when the
   *  handle was restored from a cached snapshot. */
  readonly bootstrapped: boolean;
  /** ADR-0065: how the attested reset of the journal was answered, on a
   *  founding that carried one. */
  readonly journalReset?: "reset" | "already-reset";
}

/**
 * Resolve the local DeviceGroup state: either restore the
 * cached snapshot or bootstrap a fresh founder leaf.
 *
 * Idempotent on the snapshot side — calling twice with the
 * same `(accountIdB64, deviceIdentitySeed, deviceKey)` triple
 * returns the same group (the bootstrap branch fires only
 * when the snapshot is absent).
 *
 * @param accountIdBytes - Raw 16-byte UUIDv7.
 * @param accountIdB64 - Base64 (Foundation-style padded) form of `accountIdBytes`.
 * @param deviceIdentitySeed - The 32-byte random seed cached
 *   by `ensureDeviceIdentitySeed` in `device-identity-store.ts`.
 * @param deviceKey - Non-extractable AES-GCM CryptoKey from
 *   `deriveDeviceRootKey` — used to unwrap the snapshot
 *   envelope in IDB.
 */
export async function ensureDeviceGroup(
  accountIdBytes: Uint8Array,
  accountIdB64: string,
  deviceIdentitySeed: Uint8Array,
  deviceKey: CryptoKey,
  /** ADR-0057 — the account's phrase seed, so a founding can claim (or, on a
   *  re-found, reset) the journal with the account's root even before the
   *  session has installed its signer factory. Optional: without it the
   *  registry's root is used, and failing that the first refused request
   *  claims. */
  accountIdentitySeed?: Uint8Array,
  options: EnsureDeviceGroupOptions = {},
): Promise<EnsuredDeviceGroup> {
  const wasm = await loadWasm();
  const keyResetAttestB64 = options.keyResetAttestB64;
  if (keyResetAttestB64 !== undefined && accountIdentitySeed === undefined) {
    throw new Error("ds: a key-reset founding needs the account seed for the new root");
  }

  // Build the per-device Identity from the cached seed. This
  // is the DeviceIdentity (ADR-0007), NOT the AccountIdentity
  // ChatProvider holds. Distinct OpenMLS provider, distinct
  // signature keys.
  const identity = wasm.Identity.fromSeed(deviceIdentitySeed);

  // Try to restore prior MLS state.
  // A snapshot wrapped under a previous device key (the password-recover
  // path rotates the key) does not open; that is a fresh start for this
  // device's DeviceGroup, not a fatal error, and the old row is replaced.
  let snapshot: Uint8Array | null = null;
  try {
    // A key-reset founding never resumes a snapshot: whatever is stored is
    // the old generation's group.
    snapshot = keyResetAttestB64 === undefined ? await getDeviceGroupSnapshot(accountIdB64, deviceKey) : null;
  } catch (err) {
    console.warn("DeviceGroup snapshot unreadable under this device key, bootstrapping", err);
    snapshot = null;
  }
  if (snapshot !== null && snapshot.length > 0) {
    try {
      identity.restoreStorage(snapshot);
      const loaded = wasm.loadDeviceGroup(identity, accountIdBytes);
      if (loaded !== undefined) {
        return { identity, group: loaded, bootstrapped: false };
      }
      // Snapshot existed but didn't carry the DeviceGroup
      // row — fall through to bootstrap. Defensive: this
      // shouldn't happen in normal flows because every
      // snapshot we write was captured immediately after a
      // DeviceGroup mutation.
    } catch (err) {
      // Decrypt / restore failed (device key rotated since
      // the snapshot was written, ciphertext tampered, …).
      // Fall through to bootstrap; the recovered device key
      // produces a new founding leaf rather than the prior
      // one, which is the canonical "fresh-device-on-recovery"
      // behaviour.
      // eslint-disable-next-line no-console
      console.warn("DeviceGroup snapshot restore failed, bootstrapping", err);
    }
  }

  // Bootstrap. Local device becomes the founding leaf of the
  // user's DeviceGroup.
  const group = wasm.DeviceGroup.createFounder(identity, accountIdBytes);
  // ADR-0057 obligations 1 + 7 — the account owns its DeviceGroup, so every
  // device holds its root. Claim the journal at founding with it; a 409 means
  // the id already carries an older DeviceGroup's chain (this is a re-found
  // after a lost-state recovery), and the root then RESETS it.
  //
  // FAIL-CLOSED (the second security pass, finding 3): the claim (or reset)
  // must land BEFORE the founding is persisted, and so before any device is
  // added and any Welcome sent. A failed claim, or no root available yet,
  // throws: nothing is persisted, the in-memory founder is dropped, and the
  // next call founds and claims again.
  //
  // ADR-0065: after a key reset the claim cannot be won (the journal is held
  // by the OLD root), so the founding sends the attested reset instead.
  let journalReset: "reset" | "already-reset" | undefined;
  try {
    const founding = takeSigner(() => group.dsAccessSigner(identity));
    if (founding === null) throw new Error("ds: the DeviceGroup founding key could not be derived");
    const groupId = group.groupId();
    let authority = dsAccess.journalAuthority(groupId);
    // A key-reset founding always derives the root from the seed it was
    // given (the NEW one): the registry may still answer with the root of
    // the generation being replaced.
    if ((authority.kind !== "root" || keyResetAttestB64 !== undefined) && accountIdentitySeed !== undefined) {
      releaseAuthority(authority);
      const account = wasm.Identity.fromSeed(accountIdentitySeed);
      try {
        authority = {
          kind: "root",
          signer: wasm.dsJournalRootSigner(account, groupId),
          rootSeed: wasm.dsJournalRootSeed(account.ed25519Seed, groupId),
        };
      } finally {
        account.free();
      }
    }
    try {
      if (authority.kind !== "root") {
        throw new Error("ds: the DeviceGroup root is not available yet");
      }
      if (keyResetAttestB64 !== undefined) {
        // ADR-0066 section 4.6: the new account key endorses the root that
        // becomes the journal's (the Reset statement's signer), bound to the
        // journal and the attested generation.
        const rootEndorsementB64 = rootEndorsementFor(
          wasm,
          accountIdentitySeed,
          groupId,
          keyResetAttestB64,
          authority.signer.publicKey(),
        );
        journalReset = await resetWithAttestation(
          new DsClient(),
          groupId,
          authority.signer,
          founding.epoch,
          founding.verifyingKey(),
          keyResetAttestB64,
          rootEndorsementB64,
        );
      } else {
        await claimOrReset(new DsClient(), groupId, authority.signer, founding.epoch, founding.verifyingKey());
      }
    } finally {
      releaseAuthority(authority);
      founding.free();
    }
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("ds: DeviceGroup claim at founding failed; not founded", err);
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
    throw err;
  }
  const fresh = identity.snapshotStorage();
  await putDeviceGroupSnapshot(accountIdB64, fresh, deviceKey);
  return journalReset === undefined
    ? { identity, group, bootstrapped: true }
    : { identity, group, bootstrapped: true, journalReset };
}

/**
 * ADR-0066 section 4.6 - `root_endorsement_b64` for an attested DS statement:
 * `signDsRootEndorsement(newAccountEd25519Seed, journalId, keyGeneration,
 * rootKey)` in STANDARD base64, where the generation is read off the
 * attestation the statement carries and `rootKey` is the key that BECOMES the
 * root (a reset's signer, a transfer's `new_root`).
 */
export function rootEndorsementFor(
  wasm: Pick<Awaited<ReturnType<typeof loadWasm>>, "Identity" | "signDsRootEndorsement">,
  newAccountIdentitySeed: Uint8Array | undefined,
  journalId: Uint8Array,
  keyResetAttestB64: string,
  rootKey: Uint8Array,
): string {
  if (newAccountIdentitySeed === undefined) {
    throw new Error("ds: a root endorsement needs the new account seed");
  }
  const generation = keyResetAttestationGeneration(base64Decode(keyResetAttestB64));
  const account = wasm.Identity.fromSeed(newAccountIdentitySeed);
  try {
    return base64Encode(wasm.signDsRootEndorsement(account.ed25519Seed, journalId, generation, rootKey));
  } finally {
    account.free();
  }
}

/**
 * Re-capture the MLS provider snapshot after a DeviceGroup
 * mutation (add-device, remove-device, application-message
 * send). Call site pattern:
 *
 * ```ts
 *   const { identity, group } = await ensureDeviceGroup(...);
 *   group.addDeviceFromWire(identity, kp);   // mutates state
 *   await persistDeviceGroupSnapshot(identity, accountIdB64, deviceKey);
 * ```
 *
 * Failures log + swallow — a snapshot persist failure
 * shouldn't tear down the in-memory state.
 */
export async function persistDeviceGroupSnapshot(
  identity: WasmIdentity,
  accountIdB64: string,
  deviceKey: CryptoKey,
): Promise<void> {
  try {
    const snapshot = identity.snapshotStorage();
    await putDeviceGroupSnapshot(accountIdB64, snapshot, deviceKey);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("DeviceGroup snapshot persist failed", err);
  }
}

/**
 * The receive loop's persist: the snapshot AND the journal position it has
 * consumed up to, in one write, and FAIL-CLOSED — unlike
 * {@link persistDeviceGroupSnapshot} this rejects on failure, because the loop
 * must hold its cursor when the write did not land (see
 * `sync/journal-batch.ts`).
 */
export async function persistDeviceGroupSnapshotWithCursor(
  identity: WasmIdentity,
  accountIdB64: string,
  deviceKey: CryptoKey,
  journalCursor: number,
): Promise<void> {
  const snapshot = identity.snapshotStorage();
  await putDeviceGroupSnapshot(accountIdB64, snapshot, deviceKey, journalCursor);
}

/**
 * ADR-0053 contract point 3 for a one-shot DeviceGroup commit (add device,
 * remove device, the attested-removal policy): register the key of the epoch
 * the commit just moved `group` to, signed with the account's ROOT (ADR-0057:
 * every device of the account holds it). `_signerN`, the key taken BEFORE the
 * create call, still proves the publish that follows; it registers nothing.
 * THROWS on a refusal; the caller then aborts before persisting or
 * publishing, which leaves the durable DeviceGroup at N (these paths hold an
 * ephemeral group that is freed without being written).
 */
export async function registerDeviceGroupEpochOrThrow(
  ds: DsClient,
  group: WasmDeviceGroup,
  identity: WasmIdentity,
  _signerN: ProofSigner | null,
): Promise<void> {
  const signerN1 = takeSigner(() => group.dsAccessSigner(identity));
  try {
    await registerBeforePublish(ds, group.groupId(), signerN1);
  } finally {
    signerN1?.free();
  }
}
