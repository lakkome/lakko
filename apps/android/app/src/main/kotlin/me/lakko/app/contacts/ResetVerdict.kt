// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import me.lakko.app.transparency.LogVerifyStatus

/**
 * ADR-0073 R.2 with rule 4 (amended 2026-11-26, the second time) - the
 * acceptance rule for a contact's account-key RESET.
 *
 * The table is the core's, written once in `lakko_contacts::reset_verdict`
 * and reached through [ResetRule] (`resetVerdict` on the UniFFI surface);
 * this file used to carry a hand-written copy of it and no longer does. What
 * is left here is the vocabulary [ContactsClient] gathers the inputs in and
 * reads the outcome in, and the conversion to and from the generated
 * records.
 *
 * Four outcomes:
 *
 *   - **sealed**: rules 1 to 4 hold. The same person: re-pin, and everything
 *     ADR-0101 moves (conversations, profile, history, channel returns) runs.
 *     Nothing is shown.
 *   - **unsealed**: rules 1 to 3 hold and rule 4 does not (no seal, an
 *     invalid one, or one whose key the previous announcement does not
 *     list). A new account under the same username (ADR-0101 section 11, UX
 *     16.11): no re-pin onto the old contact, nothing moved, nothing
 *     returned.
 *   - **refused**: the fail-closed refusal, as before.
 *   - **no verdict**: something could not be reached; nothing changes.
 *
 * The host JVM loads no native library, so its suite injects a fake rule
 * and tests what [ContactsClient] does with each outcome.
 */

/** What the caller learned by running `verifyKeyResetAttestation` over the
 *  bundle's `key_reset_attest` under the trusted attest key. */
public sealed interface AttestCheck {
    /** The bundle carries no attestation. */
    public data object Absent : AttestCheck

    /** It carries one and the verifier refused it, or it is not decodable. */
    public data object Invalid : AttestCheck

    /** No attest key could be obtained (no build-time pin and the JWKS fetch
     *  failed): nothing can be said about the attestation yet. */
    public data object NoKey : AttestCheck

    /** It verified for this account id and the fetched key. */
    public data class Verified(val keyGeneration: Long, val signedAtMs: Long) : AttestCheck
}

/** The verified key-log answer for the fetched key (`verifyIdentityInLog`). */
public data class LogCheck(
    val status: LogVerifyStatus,
    /** The log's `is_reset` marker when a latest entry was read. */
    val isReset: Boolean?,
)

/** Rule 4's signature half (`checkResetSeal`): whether the reset seal is
 *  signed by a key the previous generation's announcement lists. */
public data class SealCheck(
    val valid: Boolean,
    /** The previous announcement's number when valid (0 otherwise). */
    val announcementSeq: Long = 0L,
    /** The core's refusal code when not valid, for a log line. */
    val refusal: String? = null,
) {
    public companion object {
        /** No seal could be checked (nothing served, or the check threw). */
        public val NONE: SealCheck = SealCheck(valid = false, refusal = "noSeal")
    }
}

/** Rule 4's key-log half: the served announcement is in the log, for this
 *  account and generation, before the reset's leaf. Ignored by the rule
 *  when the build has no key log. */
public enum class SealLogCheck { PROVEN, NOT_PROVEN, UNREACHABLE }

/** Which refusal copy the action shows (`contacts.identity_changed.*`). */
public enum class ResetRefusal {
    /** The key log shows the new key and does NOT record a reset. */
    UNEXPLAINED,

    /** Everything else that is negative. */
    UNCONFIRMED,
}

public sealed interface ResetVerdict {
    /** The same person. [announcementSeq] is the new floor of the account's
     *  announcement numbers. */
    public data class Sealed(val keyGeneration: Long, val signedAtMs: Long, val announcementSeq: Long) : ResetVerdict

    /** A new account under the same username. */
    public data class Unsealed(val keyGeneration: Long, val signedAtMs: Long) : ResetVerdict

    public data class Refused(val why: ResetRefusal) : ResetVerdict
    public data object NoVerdict : ResetVerdict
}

/** The facts of one fetched bundle the rule reads. */
public data class ResetVerdictInput(
    /** The key this device HOLDS for the account (the pin, or a verified
     *  binding for a member it never pinned), standard base64. */
    val heldKeyB64: String,
    /** The generation this device last accepted for the account; 0 when none
     *  was recorded. */
    val acceptedGeneration: Long,
    /** The fetched bundle's key and generation. */
    val fetchedKeyB64: String,
    val bundleGeneration: Long,
    val attest: AttestCheck,
    val log: LogCheck,
    val seal: SealCheck = SealCheck.NONE,
    val sealLog: SealLogCheck = SealLogCheck.NOT_PROVEN,
)

/** R.2 with rule 4. The production rule is the core's ([NativeResetRule]). */
public fun interface ResetRule {
    public fun verdict(input: ResetVerdictInput): ResetVerdict
}

/** `lakko_contacts::reset_verdict` over UniFFI. */
public object NativeResetRule : ResetRule {
    override fun verdict(input: ResetVerdictInput): ResetVerdict {
        val held = try {
            java.util.Base64.getDecoder().decode(input.heldKeyB64)
        } catch (_: IllegalArgumentException) {
            return ResetVerdict.Refused(ResetRefusal.UNCONFIRMED)
        }
        val fetched = try {
            java.util.Base64.getDecoder().decode(input.fetchedKeyB64)
        } catch (_: IllegalArgumentException) {
            return ResetVerdict.Refused(ResetRefusal.UNCONFIRMED)
        }
        val out = uniffi.lakko_ffi_uniffi.resetVerdict(
            uniffi.lakko_ffi_uniffi.FfiResetVerdictInput(
                heldKey = held,
                acceptedGeneration = input.acceptedGeneration.coerceAtLeast(0L).toULong(),
                fetchedKey = fetched,
                bundleGeneration = input.bundleGeneration.coerceAtLeast(0L).toULong(),
                attest = when (val a = input.attest) {
                    AttestCheck.Absent -> uniffi.lakko_ffi_uniffi.FfiAttestCheck.Absent
                    AttestCheck.Invalid -> uniffi.lakko_ffi_uniffi.FfiAttestCheck.Invalid
                    AttestCheck.NoKey -> uniffi.lakko_ffi_uniffi.FfiAttestCheck.NoKey
                    is AttestCheck.Verified ->
                        if (a.keyGeneration < 0) {
                            uniffi.lakko_ffi_uniffi.FfiAttestCheck.Invalid
                        } else {
                            uniffi.lakko_ffi_uniffi.FfiAttestCheck.Verified(a.keyGeneration.toULong(), a.signedAtMs)
                        }
                },
                logStatus = when (input.log.status) {
                    LogVerifyStatus.CONFIRMED -> uniffi.lakko_ffi_uniffi.FfiLogStatus.CONFIRMED
                    LogVerifyStatus.ABSENT -> uniffi.lakko_ffi_uniffi.FfiLogStatus.ABSENT
                    LogVerifyStatus.ROLLBACK -> uniffi.lakko_ffi_uniffi.FfiLogStatus.ROLLBACK
                    LogVerifyStatus.MISMATCH -> uniffi.lakko_ffi_uniffi.FfiLogStatus.MISMATCH
                    LogVerifyStatus.UNREACHABLE -> uniffi.lakko_ffi_uniffi.FfiLogStatus.UNREACHABLE
                    LogVerifyStatus.OFF -> uniffi.lakko_ffi_uniffi.FfiLogStatus.OFF
                },
                logIsReset = input.log.isReset,
                seal = uniffi.lakko_ffi_uniffi.FfiSealCheck(
                    valid = input.seal.valid,
                    announcementSeq = input.seal.announcementSeq.coerceAtLeast(0L).toULong(),
                    kind = null,
                    refusal = input.seal.refusal,
                ),
                sealLog = when (input.sealLog) {
                    SealLogCheck.PROVEN -> uniffi.lakko_ffi_uniffi.FfiSealLog.PROVEN
                    SealLogCheck.NOT_PROVEN -> uniffi.lakko_ffi_uniffi.FfiSealLog.NOT_PROVEN
                    SealLogCheck.UNREACHABLE -> uniffi.lakko_ffi_uniffi.FfiSealLog.UNREACHABLE
                },
            ),
        )
        return when (out) {
            is uniffi.lakko_ffi_uniffi.FfiResetVerdict.Sealed ->
                ResetVerdict.Sealed(out.keyGeneration.toLong(), out.signedAtMs, out.announcementSeq.toLong())
            is uniffi.lakko_ffi_uniffi.FfiResetVerdict.Unsealed ->
                ResetVerdict.Unsealed(out.keyGeneration.toLong(), out.signedAtMs)
            is uniffi.lakko_ffi_uniffi.FfiResetVerdict.Refused -> ResetVerdict.Refused(
                when (out.why) {
                    uniffi.lakko_ffi_uniffi.FfiResetRefusal.UNEXPLAINED -> ResetRefusal.UNEXPLAINED
                    uniffi.lakko_ffi_uniffi.FfiResetRefusal.UNCONFIRMED -> ResetRefusal.UNCONFIRMED
                },
            )
            uniffi.lakko_ffi_uniffi.FfiResetVerdict.NoVerdict -> ResetVerdict.NoVerdict
        }
    }
}

/**
 * The rotation marker of the re-pin (R.2 "The re-pin"): never backwards, in
 * ADR-0016's marker space (seconds).
 */
public fun resetRepinMarker(heldMarker: Long, nowMs: Long): Long =
    maxOf(heldMarker, Math.floorDiv(nowMs, 1000L))
