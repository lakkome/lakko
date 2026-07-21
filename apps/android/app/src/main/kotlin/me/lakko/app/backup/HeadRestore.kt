// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import android.content.Context
import android.util.Log
import me.lakko.app.devices.DeviceAttestKey
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.backupEnvelopeHash
import uniffi.lakko_ffi_uniffi.parseBackupBlob
import uniffi.lakko_ffi_uniffi.verifyBackupHead
import uniffi.lakko_ffi_uniffi.verifyBackupHeadBinding

/**
 * Backup-head restore evaluation — the IO that resolves the [RestoreGateFacts]
 * (ADR-0037 §6, G-11) and applies the pure [restoreVerdict] policy.
 *
 * Mirror of `apps/web/src/backup/head-restore.ts`.
 *
 * This is the impure half of the restore gate: it peeks the storage blob, parses
 * the framing, computes the envelope hash, fetches the pinned attest key, reads
 * the persisted floor, and runs the FFI verify — then hands the resolved facts
 * to [restoreVerdict]. Kept separate from the pure classifier so the security
 * policy stays unit-testable without FFI / Keystore / network.
 */

/** The resolved outcome of evaluating a storage blob against the restore gate. */
public data class RestoreEvaluation(
    /** The terminal verdict from [restoreVerdict]. */
    val verdict: RestoreVerdict,
    /** The envelope portion to open with the phrase (the whole blob for a legacy
     *  blob; the tail after `TAG ‖ head` for a framed one). */
    val envelope: ByteArray,
    /** `backupEnvelopeHash(envelope)` — carried so a clean [RestoreVerdict.Proceed]
     *  can persist the floor without recomputing. `null` when the blob failed to
     *  parse, on a corrupt-floor short-circuit, or on a zero-anchor absent-head
     *  fail-open; set for a framed head and for the F4 absent-with-floor case. */
    val envelopeHash: ByteArray?,
    /** The verified `backup_version` on a clean [RestoreVerdict.Proceed], else
     *  `null`. Used to advance the monotone floor. */
    val verifiedVersion: ULong?,
)

/**
 * Evaluate a storage blob against the ADR-0037 §6 restore gate.
 *
 * Resolves each fact via IO then returns the [restoreVerdict]. The caller
 * ([BackupClient.restore]) acts on the verdict: [RestoreVerdict.HardFail] →
 * refuse + alarm; [RestoreVerdict.StaleOverride] → ask the user;
 * [RestoreVerdict.FailOpen] / [RestoreVerdict.Proceed] → open the envelope (and,
 * on a clean proceed, persist the floor from `verifiedVersion` + `envelopeHash`).
 *
 * @param context      Android context for the [FloorStore] read.
 * @param blob         the raw bytes fetched from `GET /backup`.
 * @param accountId    the 16-byte account_id the client authenticated as.
 * @param accountIdB64 the floor-store key (base64 of the account_id).
 * @param nowMillis    injectable clock in ms.
 * @param maxStalenessMillis the freshness window in ms.
 */
public suspend fun evaluateRestoreBlob(
    context: Context,
    blob: ByteArray,
    accountId: ByteArray,
    accountIdB64: String,
    nowMillis: Long,
    maxStalenessMillis: Long,
): RestoreEvaluation {
    // 1. Parse the framing. A truncated / corrupt FRAMED blob throws → hard-fail
    //    (active tamper). A legacy / empty blob parses as unframed (Legacy).
    //    F3: catch `Throwable`, not just `FfiException` — a non-FFI throwable
    //    (a JNI / panic `RuntimeException`) must become the `parseError` fact
    //    (a clean HardFail + alarm), never escape the gated restore uncaught
    //    (parity with the attest-key handler below).
    val parsed = try {
        parseBackupBlob(blob)
    } catch (_: Throwable) {
        return RestoreEvaluation(
            verdict = restoreVerdict(RestoreGateFacts(parseError = true)),
            envelope = blob,
            envelopeHash = null,
            verifiedVersion = null,
        )
    }

    val envelope = parsed.envelope
    val head = parsed.head

    // 2. Read the persisted floor as a discriminated Absent / Present / Corrupt
    //    (F5a). A CORRUPT anchor dominates every framing case below — the local
    //    anchor once existed and is now untrustworthy, so route it to
    //    stale-override, never silently demoting to the weaker fresh path.
    val floorRead = FloorStore.getFloor(context, accountIdB64)
    val hasFloor = floorRead is FloorRead.Present
    val floorCorrupt = floorRead is FloorRead.Corrupt
    val floor = (floorRead as? FloorRead.Present)?.floor

    if (floorCorrupt) {
        return RestoreEvaluation(
            verdict = restoreVerdict(
                RestoreGateFacts(framed = parsed.framed, floorCorrupt = true),
            ),
            envelope = envelope,
            envelopeHash = null,
            verifiedVersion = null,
        )
    }

    // 3. Absent head (F4). Split on the floor: no floor → fail-open (true
    //    zero-anchor fresh install); floor + byte-identical envelope → proceed;
    //    floor + a DIFFERENT headless envelope → stale-override (head-stripping
    //    downgrade against head-protected history). The phrase-AEAD still
    //    protects the content in every case — invariant #4 untouched.
    if (!parsed.framed || head == null) {
        if (!hasFloor) {
            return RestoreEvaluation(
                verdict = restoreVerdict(
                    RestoreGateFacts(framed = false, hasFloor = false),
                ),
                envelope = envelope,
                envelopeHash = null,
                verifiedVersion = null,
            )
        }
        val absentEnvelopeHash = backupEnvelopeHash(envelope)
        val absentHashMatchesFloor =
            floor != null && absentEnvelopeHash.contentEquals(floor.envelopeHash)
        return RestoreEvaluation(
            verdict = restoreVerdict(
                RestoreGateFacts(
                    framed = false,
                    hasFloor = true,
                    absentHashMatchesFloor = absentHashMatchesFloor,
                ),
            ),
            envelope = envelope,
            envelopeHash = absentEnvelopeHash,
            verifiedVersion = null,
        )
    }

    val envelopeHash = backupEnvelopeHash(envelope)

    // 4. A framed head we cannot verify (no pinned + no JWKS attest key) is a
    //    misconfig, NOT an absent head — fail-closed (hard), never silently
    //    accept an unverified framed head (ADR-0037 §6).
    val attestKey = try {
        DeviceAttestKey.fetch()
    } catch (_: Throwable) {
        return RestoreEvaluation(
            verdict = RestoreVerdict.HardFail(
                "attest key unavailable - cannot verify the backup head",
            ),
            envelope = envelope,
            envelopeHash = envelopeHash,
            verifiedVersion = null,
        )
    }

    // F1: the rollback guarantee is only as strong as the verifying key. When it
    //     came from the JWKS endpoint (pin unset) rather than the build-time pin,
    //     a compromised auth origin could serve a forged key + head — surface the
    //     weakened posture loudly. Warn only; do NOT hard-fail (that would brick
    //     unpinned rollouts — the head is still verified, just against a
    //     less-trusted key). A `null` pin after a successful `fetch` means the key
    //     was JWKS-sourced (a pinned key wins outright in `fetch` before the
    //     endpoint is consulted — race-free).
    if (DeviceAttestKey.pinnedKey() == null) {
        Log.w(
            "lakko-backup",
            "[backup] backup-head verified against an UNPINNED (JWKS-served) " +
                "attest key - set LAKKO_DEVICE_ATTEST_KEY for full rollback protection",
        )
    }

    // 5. Framed + valid floor state.
    if (hasFloor && floor != null) {
        // Returning client: signature + account binding only, then hash + floor.
        return try {
            val verified = verifyBackupHeadBinding(head, attestKey, accountId)
            val hashMismatch = !verified.envelopeHash.contentEquals(envelopeHash)
            val versionBelowFloor = verified.backupVersion < floor.version
            val verdict = restoreVerdict(
                RestoreGateFacts(
                    framed = true,
                    hasFloor = true,
                    hashMismatch = hashMismatch,
                    versionBelowFloor = versionBelowFloor,
                ),
            )
            RestoreEvaluation(
                verdict = verdict,
                envelope = envelope,
                envelopeHash = envelopeHash,
                verifiedVersion = if (verdict is RestoreVerdict.Proceed) {
                    verified.backupVersion
                } else {
                    null
                },
            )
        } catch (_: FfiException) {
            RestoreEvaluation(
                verdict = restoreVerdict(
                    RestoreGateFacts(
                        framed = true,
                        hasFloor = true,
                        bindingInvalid = true,
                    ),
                ),
                envelope = envelope,
                envelopeHash = envelopeHash,
                verifiedVersion = null,
            )
        }
    }

    // Fresh install: fold signature + account + hash + freshness into one call.
    return try {
        val verified = verifyBackupHead(
            head,
            attestKey,
            accountId,
            envelopeHash,
            nowMillis,
            maxStalenessMillis,
        )
        RestoreEvaluation(
            verdict = restoreVerdict(RestoreGateFacts(framed = true, hasFloor = false)),
            envelope = envelope,
            envelopeHash = envelopeHash,
            verifiedVersion = verified.backupVersion,
        )
    } catch (_: FfiException.BackupHeadStale) {
        RestoreEvaluation(
            verdict = restoreVerdict(
                RestoreGateFacts(framed = true, hasFloor = false, freshStale = true),
            ),
            envelope = envelope,
            envelopeHash = envelopeHash,
            verifiedVersion = null,
        )
    } catch (_: FfiException) {
        // Any non-stale failure (including `Backup`) is a hard tamper.
        RestoreEvaluation(
            verdict = restoreVerdict(
                RestoreGateFacts(framed = true, hasFloor = false, freshInvalid = true),
            ),
            envelope = envelope,
            envelopeHash = envelopeHash,
            verifiedVersion = null,
        )
    }
}
