// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import me.lakko.app.transparency.LogVerifyStatus

/**
 * A stand-in for the core's reset rule (`lakko_contacts::reset_verdict`,
 * reached in the app through [NativeResetRule]) for the host JVM, which
 * loads no native library. It follows the core's order of checks so the
 * gate tests can drive every outcome; the table itself is tested in Rust,
 * and this file is not a second definition of it: no production code reads
 * it.
 */
object CoreResetRuleDouble : ResetRule {
    override fun verdict(input: ResetVerdictInput): ResetVerdict {
        if (input.fetchedKeyB64 == input.heldKeyB64) return ResetVerdict.NoVerdict
        val refusal = ResetVerdict.Refused(
            if (input.log.status == LogVerifyStatus.CONFIRMED && input.log.isReset == false) {
                ResetRefusal.UNEXPLAINED
            } else {
                ResetRefusal.UNCONFIRMED
            },
        )
        val attest = when (val a = input.attest) {
            AttestCheck.Absent, AttestCheck.Invalid -> return refusal
            AttestCheck.NoKey -> return ResetVerdict.NoVerdict
            is AttestCheck.Verified -> a
        }
        if (attest.keyGeneration != input.bundleGeneration || attest.keyGeneration <= input.acceptedGeneration) {
            return refusal
        }
        val logOff = when (input.log.status) {
            LogVerifyStatus.CONFIRMED -> if (input.log.isReset == true) false else return refusal
            LogVerifyStatus.MISMATCH, LogVerifyStatus.ROLLBACK -> return refusal
            LogVerifyStatus.OFF -> true
            LogVerifyStatus.UNREACHABLE, LogVerifyStatus.ABSENT -> return ResetVerdict.NoVerdict
        }
        val unsealed = ResetVerdict.Unsealed(attest.keyGeneration, attest.signedAtMs)
        if (!input.seal.valid) return unsealed
        val sealed = ResetVerdict.Sealed(attest.keyGeneration, attest.signedAtMs, input.seal.announcementSeq)
        if (logOff) return sealed
        return when (input.sealLog) {
            SealLogCheck.PROVEN -> sealed
            SealLogCheck.NOT_PROVEN -> unsealed
            SealLogCheck.UNREACHABLE -> ResetVerdict.NoVerdict
        }
    }
}
