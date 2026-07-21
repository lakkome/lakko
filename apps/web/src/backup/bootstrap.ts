// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Post-auth backup bootstrap (Phase 2a1c).
 *
 * Called from the AuthContext's signup + recover handlers right
 * after the canonical Identity lands, the device root key is
 * derived, and the session token is bound. The phrase is on
 * hand here (signup just minted it, recover just consumed it),
 * which is the only moment we can cache it under the device-
 * root key and post a baseline backup envelope to the server.
 *
 * Login does NOT call this function — the phrase isn't
 * available there. A returning user who wants to enable backups
 * on a new device runs through `recover` first.
 *
 * Best-effort: every operation here is wrapped so a failure
 * logs and continues. A broken phrase cache or a transient
 * server hiccup must not block auth from flipping into the
 * Session state.
 */
import { base64Encode } from "../auth/account-id";
import { BackupClient } from "./client";
import { putPhrase } from "./phrase-store";

export interface BackupBootstrapInputs {
  /** Raw 16-byte UUIDv7 account id. */
  readonly accountId: Uint8Array;
  /** Base64-encoded account id. Provided alongside the raw bytes
   *  because callers already have both; threading both avoids a
   *  second base64-encode here. */
  readonly accountIdB64: string;
  /** 32-byte identity signing seed — same value
   *  `canonical.ed25519Seed` reports on the wasm `Identity` handle. */
  readonly ed25519Seed: Uint8Array;
  /** Non-extractable AES-GCM `CryptoKey` from
   *  `deriveDeviceRootKey`. Used to wrap the phrase for the
   *  IDB cache. */
  readonly deviceKey: CryptoKey;
  /** Bearer token to authorise the `/backup` PUT. The session
   *  token already bound by the auth flow. */
  readonly sessionToken: string;
  /** UTF-8 bytes of the canonical recovery phrase. */
  readonly phraseBytes: Uint8Array;
  /** Phase 2a3d4: whole-storage MLS snapshot from
   *  `Identity.snapshotStorage()`. Empty `Uint8Array` is fine
   *  on signup baseline; recover-time path passes the snapshot
   *  it just restored so the new baseline upload doesn't nuke
   *  the prior server-side MLS state. */
  readonly wholeStorageSnapshot?: Uint8Array;
}

/**
 * Cache the phrase under the device root key + post a baseline
 * backup envelope to the server. Best-effort end-to-end — any
 * single step failing is logged and does NOT propagate.
 *
 * The phrase bytes are NOT zeroed by this function; the caller
 * is responsible for the lifetime of the input Uint8Array. The
 * function itself does not retain any reference past return.
 */
export async function bootstrapBackup(inputs: BackupBootstrapInputs): Promise<void> {
  // 1. Cache the phrase locally so future auto-backups (post-
  //    schedule, on local-state change, etc.) can run without
  //    asking the user for the phrase again.
  try {
    await putPhrase(inputs.accountIdB64, inputs.phraseBytes, inputs.deviceKey);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("bootstrapBackup: putPhrase failed", err);
    // Continue — we can still upload a one-shot backup even if
    // the local phrase cache failed; just no auto-backups
    // afterwards. The next signup / recover flow re-tries the
    // cache write.
  }

  // 2. Post the baseline envelope. The server overwrites any
  //    prior blob; this is the "fresh state" snapshot every
  //    later device-restore can fall back to.
  try {
    const client = new BackupClient();
    client.setSessionToken(inputs.sessionToken);
    const result = await client.uploadNow(
      inputs.accountId,
      inputs.ed25519Seed,
      inputs.phraseBytes,
      inputs.wholeStorageSnapshot ?? new Uint8Array(0),
      // ADR-0037: hand the device key so a successful mint persists the
      // `backup_version` floor for future returning-client restores.
      inputs.deviceKey,
    );
    if (result.kind !== "uploaded") {
      // eslint-disable-next-line no-console
      console.warn("bootstrapBackup: uploadNow returned", result.kind);
    }
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("bootstrapBackup: uploadNow threw", err);
  }
}

/** Convenience — caller has a string phrase; wraps the UTF-8
 *  encode + `bootstrapBackup` in one call. The string variant
 *  is what both signup (returns phrase as a string) and recover
 *  (takes phrase as a string) naturally hold. The optional
 *  `wholeStorageSnapshot` rides through `base` if the caller
 *  has it on hand (recover after a successful server-snapshot
 *  restore). */
export async function bootstrapBackupFromPhraseString(
  base: Omit<BackupBootstrapInputs, "phraseBytes">,
  phrase: string,
): Promise<void> {
  const phraseBytes = new TextEncoder().encode(phrase);
  try {
    await bootstrapBackup({ ...base, phraseBytes });
  } finally {
    phraseBytes.fill(0);
  }
}

/** Convenience signature for the JS-string accountIdB64 helper
 *  that the callers in `AuthContext.tsx` already have on hand. */
export { base64Encode };
