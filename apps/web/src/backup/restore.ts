// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Post-recovery backup restore (Phase 2a1d).
 *
 * The recover handler in `AuthContext.tsx` lands on a new device
 * with nothing but the recovery phrase + the OPAQUE-recovered
 * session token. The user's prior accepted-contacts list lives
 * only inside the encrypted backup envelope that the previous
 * device PUT to `/backup`.
 *
 * This helper fetches that envelope, decrypts it with the phrase,
 * and applies the restored accepted-contacts list to the local
 * store BEFORE the post-recovery bootstrap re-uploads a fresh
 * baseline. The ordering is critical: the bootstrap-side
 * `composeEnvelope` reads `loadAccepted()` at upload time, so
 * applying the restored contacts first means the new baseline
 * carries the merged set instead of nuking the prior backup with
 * an empty list.
 *
 * Best-effort end-to-end. A `no-backup` outcome (the user never
 * uploaded one) is expected on the very first recover; a
 * `wrong-phrase-or-tampered` outcome should not block the flow —
 * the user can still continue, just without their prior contacts.
 * Network failures are logged and the bootstrap continues.
 */
import {
  applyRestoredContacts,
  BackupClient,
  type RestoreGateContext,
  type RestoreResult,
} from "./client";

export interface BackupRestoreInputs {
  /** Bearer token to authorise the `/backup` GET. The session
   *  token bound by the recover flow. */
  readonly sessionToken: string;
  /** ADR-0037 §6 rollback gate: verify the auth-signed backup head before
   *  opening the envelope. **Required** (F2) — the returning-client hard floor +
   *  fresh-install freshness policy always applies; there is no ungated bypass. */
  readonly gate: RestoreGateContext;
}

/**
 * Restore the prior backup blob for the authenticated account.
 *
 * On a `restored` outcome, upserts the prior accepted-contacts
 * into the local store. The caller still observes the discriminated
 * `RestoreResult` so it can log diagnostics for the non-restored
 * branches.
 *
 * The phrase bytes are NOT zeroed by this function; the caller
 * is responsible for the lifetime of the input Uint8Array. The
 * function itself does not retain any reference past return.
 */
export async function restoreFromServerBackup(
  inputs: BackupRestoreInputs,
  phraseBytes: Uint8Array,
): Promise<RestoreResult> {
  const client = new BackupClient();
  client.setSessionToken(inputs.sessionToken);
  const result = await client.restore(phraseBytes, inputs.gate);
  if (result.kind === "restored") {
    applyRestoredContacts(result.acceptedContacts);
  }
  return result;
}

/** Convenience: caller has a string phrase; wraps the UTF-8
 *  encode + scrub in one call. Mirrors the `bootstrap.ts`
 *  `bootstrapBackupFromPhraseString` helper. */
export async function restoreFromServerBackupFromPhraseString(
  inputs: BackupRestoreInputs,
  phrase: string,
): Promise<RestoreResult> {
  const phraseBytes = new TextEncoder().encode(phrase);
  try {
    return await restoreFromServerBackup(inputs, phraseBytes);
  } finally {
    phraseBytes.fill(0);
  }
}
