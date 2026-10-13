// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import me.lakko.app.transparency.LogVerifyStatus

/**
 * ADR-0073 "Phase 2 revised: silent continuity", R.2 - the acceptance rule
 * for a contact's account-key RESET, as one pure decision.
 *
 * A reset is a key change with NO supersession proof (with one, ADR-0016's
 * path runs and this file is never asked: a proven rotation is not a
 * reset). It is accepted, silently, when the directory serves an
 * auth-signed key-reset attestation for exactly this account and this key,
 * at a generation strictly above the one this device last accepted, and the
 * key log (when one is configured) agrees. Anything negative is the
 * existing fail-closed refusal; anything not yet known is no verdict at
 * all.
 *
 * Free of the native library, storage and the network so the host suite
 * runs the whole table; [ContactsClient] gathers the inputs and acts on the
 * outcome. Mirror of `apps/web/src/contacts/reset-verdict.ts` (R.12).
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

/** Which refusal copy the action shows (`contacts.identity_changed.*`). */
public enum class ResetRefusal {
    /** The key log shows the new key and does NOT record a reset. */
    UNEXPLAINED,

    /** Everything else that is negative. */
    UNCONFIRMED,
}

public sealed interface ResetVerdict {
    public data class Accepted(val keyGeneration: Long, val signedAtMs: Long) : ResetVerdict
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
)

/** The refusal copy for a negative verdict: `unexplained` only when the log
 *  itself, read to the end, shows the key without a reset. */
private fun refusalFor(log: LogCheck): ResetVerdict =
    ResetVerdict.Refused(
        if (log.status == LogVerifyStatus.CONFIRMED && log.isReset == false) {
            ResetRefusal.UNEXPLAINED
        } else {
            ResetRefusal.UNCONFIRMED
        },
    )

/**
 * R.2, in the order the checks decide:
 *
 * | # | attestation | generation                  | key log                        | verdict                 |
 * |---|-------------|-----------------------------|--------------------------------|-------------------------|
 * | 1 | absent      | -                           | any                            | refused                 |
 * | 2 | invalid     | -                           | any                            | refused                 |
 * | 3 | no-key      | -                           | any                            | no-verdict              |
 * | 4 | verified    | != bundle's, or <= accepted | any                            | refused                 |
 * | 5 | verified    | ok                          | confirmed, `is_reset` false    | refused (`unexplained`) |
 * | 6 | verified    | ok                          | mismatch / rollback            | refused                 |
 * | 7 | verified    | ok                          | unreachable / absent           | no-verdict              |
 * | 8 | verified    | ok                          | confirmed + `is_reset`, or off | accepted                |
 *
 * A refusal's copy is `unexplained` exactly when the log is `confirmed`
 * without `is_reset`, `unconfirmed` otherwise. Row 8's `off` arm is decision
 * D3 ([logOffAcceptsReset]).
 *
 * Callers must not ask for an unchanged key; one that does is answered
 * `no-verdict` (nothing to accept, nothing to refuse).
 */
public fun resetVerdict(input: ResetVerdictInput): ResetVerdict {
    if (input.fetchedKeyB64 == input.heldKeyB64) return ResetVerdict.NoVerdict
    val attest = when (val a = input.attest) {
        AttestCheck.Absent, AttestCheck.Invalid -> return refusalFor(input.log)
        AttestCheck.NoKey -> return ResetVerdict.NoVerdict
        is AttestCheck.Verified -> a
    }
    if (attest.keyGeneration < 0 ||
        attest.keyGeneration != input.bundleGeneration ||
        attest.keyGeneration <= input.acceptedGeneration
    ) {
        return refusalFor(input.log)
    }
    val accepted = ResetVerdict.Accepted(attest.keyGeneration, attest.signedAtMs)
    return when (input.log.status) {
        LogVerifyStatus.CONFIRMED -> if (input.log.isReset == true) accepted else refusalFor(input.log)
        LogVerifyStatus.OFF -> if (logOffAcceptsReset()) accepted else ResetVerdict.Refused(ResetRefusal.UNCONFIRMED)
        LogVerifyStatus.MISMATCH, LogVerifyStatus.ROLLBACK -> refusalFor(input.log)
        LogVerifyStatus.UNREACHABLE, LogVerifyStatus.ABSENT -> ResetVerdict.NoVerdict
    }
}

/**
 * Decision D3 (R.10): with no key log configured in the build, a reset is
 * accepted on the attestation alone. `false` would refuse every reset until
 * the log is deployed.
 */
public fun logOffAcceptsReset(): Boolean = true

/**
 * The rotation marker of the re-pin (R.2 "The re-pin"): never backwards, in
 * ADR-0016's marker space (seconds).
 */
public fun resetRepinMarker(heldMarker: Long, nowMs: Long): Long =
    maxOf(heldMarker, Math.floorDiv(nowMs, 1000L))
