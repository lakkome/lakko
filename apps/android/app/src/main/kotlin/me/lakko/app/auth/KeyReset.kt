// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import me.lakko.app.discovery.DiscoveryHttpError
import me.lakko.app.ds.DsHttpError

/**
 * ADR-0073 "Reset account keys": what a reset still owes after its server
 * step, and the rules that drive the retries. The decisions are pure (a host
 * JVM test runs them without a server or the native core); the store is a
 * plain per-account row, since the attestation is a public signed artifact
 * that goes to two servers as it is.
 *
 * ## The order on the device (ADR section 2.3)
 *
 *   1. generate the phrase and the seed, derive the new identity;
 *   2. `reset/start`, `reset/finish` (the server step, one transaction: the
 *      point of no return);
 *   3. persist, before any further network call: the new seed and phrase,
 *      the new session pair, the attestation and its generation
 *      ([KeyResetOwedStore]), and the `publish-owed` marker
 *      ([RecoveryOwedStore]);
 *   4. `POST /discovery/identity` with the new bundle AND the attestation;
 *   5. the DeviceGroup: found it anew locally and re-root its journal with
 *      `PUT /ds/group/{hex}/reset` carrying the attestation;
 *   6. show the new phrase.
 *
 * Steps 4 and 5 are each retryable with the SAME attestation (a replay is
 * harmless: an applied generation cannot be applied again, and `409
 * key-generation-not-newer` means the step already landed). Nothing is ever
 * minted a second time. This device's own KeyPackage is re-published by the
 * chat bootstrap of the session landing that follows, with a device binding
 * re-signed by the new account key.
 */

/** What a reset still owes, persisted per account until both steps landed. */
@Serializable
public class KeyResetOwed(
    /** Standard base64 of the raw 145-byte attestation. */
    public val attestationB64: String,
    /** The generation the reset moved the account to. */
    public val keyGeneration: Long,
    /** The attestation's mint time, Unix milliseconds. */
    public val signedAtMs: Long,
    /** Standard base64 of the NEW AccountIdentity Ed25519 public key. */
    public val newSignaturePubkeyB64: String,
    /** `POST /discovery/identity` with the attestation landed. */
    public val discoveryPublished: Boolean = false,
    /** `PUT /ds/group/{device_group}/reset` with the attestation landed. */
    public val deviceGroupReset: Boolean = false,
) {
    public val attestation: ByteArray get() = base64Decode(attestationB64)

    public val newSignaturePubkey: ByteArray get() = base64Decode(newSignaturePubkeyB64)
}

/** The next thing a reset's retry does. */
public enum class KeyResetStep {
    DiscoveryPublish,
    DeviceGroupReset,

    /** Both landed: clear the debt and show the phrase. */
    Done,
}

/** Discovery first (ADR section 2.3: the directory before the device list),
 *  then the DeviceGroup, then nothing. */
public fun nextKeyResetStep(owed: KeyResetOwed): KeyResetStep = when {
    !owed.discoveryPublished -> KeyResetStep.DiscoveryPublish
    !owed.deviceGroupReset -> KeyResetStep.DeviceGroupReset
    else -> KeyResetStep.Done
}

/** What one step's request did. */
public sealed interface KeyResetStepOutcome {
    /** The server applied the attested change. */
    public data object Landed : KeyResetStepOutcome

    /** `409 key-generation-not-newer`: a retry of a step that already
     *  landed. Treated exactly like [Landed]. */
    public data object AlreadyLanded : KeyResetStepOutcome

    /** A transient refusal or a transport failure: the same request is
     *  retried later with the same attestation. */
    public class Retry(public val cause: Throwable, public val retryAfterSecs: Long? = null) : KeyResetStepOutcome

    /** The server refused the attestation itself (`400
     *  key-reset-attestation-invalid`): a retry answers the same. Surfaced,
     *  never retried silently. */
    public class Refused(public val cause: Throwable) : KeyResetStepOutcome
}

/** The discovery error codes the attested upload can answer. */
public const val DISCOVERY_KEY_GENERATION_NOT_NEWER: String = "key-generation-not-newer"
public const val DISCOVERY_KEY_RESET_ATTESTATION_INVALID: String = "key-reset-attestation-invalid"
public const val DISCOVERY_IDENTITY_CHANGE_NEEDS_ATTESTATION: String = "identity-change-needs-attestation"
public const val DISCOVERY_IDENTITY_CHANGED_TOO_RECENTLY: String = "identity-changed-too-recently"
public const val KEY_RESET_DISABLED: String = "key-reset-disabled"

/**
 * Classify the result of `POST /discovery/identity` with the attestation.
 * `null` [error] is a 2xx.
 */
public fun discoveryResetOutcome(error: Throwable?): KeyResetStepOutcome = when {
    error == null -> KeyResetStepOutcome.Landed
    error is DiscoveryHttpError && error.status == HTTP_CONFLICT &&
        error.code == DISCOVERY_KEY_GENERATION_NOT_NEWER -> KeyResetStepOutcome.AlreadyLanded
    error is DiscoveryHttpError && error.status == HTTP_BAD_REQUEST &&
        error.code == DISCOVERY_KEY_RESET_ATTESTATION_INVALID -> KeyResetStepOutcome.Refused(error)
    error is DiscoveryHttpError && error.status == HTTP_TOO_MANY ->
        KeyResetStepOutcome.Retry(error, error.retryAfterSecs)
    else -> KeyResetStepOutcome.Retry(error)
}

/**
 * Classify the result of `PUT /ds/group/{hex}/reset` with the attestation.
 * A `409` is "already landed" only when [rootProbeConfirmed] (the existing
 * `PUT .../root` probe naming this root answered 204); a `409` whose probe
 * did not confirm is retried. `null` [error] is a 2xx.
 */
public fun deviceGroupResetOutcome(error: Throwable?, rootProbeConfirmed: Boolean): KeyResetStepOutcome = when {
    error == null -> KeyResetStepOutcome.Landed
    error is DsHttpError && error.status == HTTP_CONFLICT && rootProbeConfirmed -> KeyResetStepOutcome.AlreadyLanded
    error is DsHttpError && error.status == HTTP_BAD_REQUEST -> KeyResetStepOutcome.Refused(error)
    else -> KeyResetStepOutcome.Retry(error)
}

/** Whether a step's outcome lets the retry move on. */
public fun keyResetStepLanded(outcome: KeyResetStepOutcome): Boolean =
    outcome is KeyResetStepOutcome.Landed || outcome is KeyResetStepOutcome.AlreadyLanded

/** A client applies a stored attestation for at most this long after its
 *  mint (ADR section 4.2, chosen not measured). */
public const val KEY_RESET_ATTESTATION_MAX_AGE_MS: Long = 30L * 24 * 60 * 60 * 1000

/** Whether the stored attestation may be used for a retry now. */
public enum class AttestationAge {
    /** Within the 30-day bound. */
    Fresh,

    /** Older than the bound: the retry waits for a fresh sign-in
     *  ("Sign in again to finish publishing your new keys"). */
    StaleNeedsSignIn,

    /** Older than the bound, but the person signed in again this process:
     *  the account is confirmed theirs, the retry proceeds. */
    StaleConfirmed,
}

/** The 30-day rule. [signedInAgainMs] is when the person last signed in
 *  this process (`null` when they did not); a sign-in after the attestation
 *  was minted confirms it. */
public fun attestationAgeAt(signedAtMs: Long, nowMs: Long, signedInAgainMs: Long?): AttestationAge = when {
    nowMs - signedAtMs <= KEY_RESET_ATTESTATION_MAX_AGE_MS -> AttestationAge.Fresh
    signedInAgainMs != null && signedInAgainMs >= signedAtMs -> AttestationAge.StaleConfirmed
    else -> AttestationAge.StaleNeedsSignIn
}

/** Why the publish of a reset is still owed, as the phrase screen shows it. */
public sealed interface PublishOwedReason {
    /** A step failed and can be retried. */
    public class Retryable(public val cause: Throwable?, public val retryAfterSecs: Long? = null) : PublishOwedReason

    /** The attestation is older than 30 days: sign in again first. */
    public data object SignInRequired : PublishOwedReason

    /** A server refused the attestation; retrying answers the same. */
    public class Refused(public val cause: Throwable) : PublishOwedReason

    /** The attestation could not be verified on this device (no pinned
     *  attest key and the JWKS fetch failed): nothing is sent with it. */
    public class Unverified(public val cause: Throwable?) : PublishOwedReason
}

/**
 * The per-account row of [KeyResetOwed]. Plain storage: the attestation is a
 * public signed artifact (two servers receive it as it is) and the rest are
 * a generation, a timestamp and a public key.
 */
public class KeyResetOwedStore(private val storage: MarkerStorage) {

    public fun load(accountIdB64: String): KeyResetOwed? {
        val raw = try {
            storage.get(KEY_PREFIX + accountIdB64)
        } catch (_: Throwable) {
            null
        } ?: return null
        return try {
            json.decodeFromString(KeyResetOwed.serializer(), raw)
        } catch (_: Throwable) {
            null
        }
    }

    public fun save(accountIdB64: String, owed: KeyResetOwed) {
        try {
            me.lakko.app.account.AccountStoreGate.write(accountIdB64) { storage.set(KEY_PREFIX + accountIdB64, json.encodeToString(KeyResetOwed.serializer(), owed)) }
        } catch (_: Throwable) {
            // The debt then lives for this process only.
        }
    }

    public fun clear(accountIdB64: String) {
        try {
            storage.remove(KEY_PREFIX + accountIdB64)
        } catch (_: Throwable) {
            // Nothing to drop.
        }
    }

    /** Record that a step landed; a no-op when no row exists. */
    public fun markStep(accountIdB64: String, step: KeyResetStep) {
        val owed = load(accountIdB64) ?: return
        val next = when (step) {
            KeyResetStep.DiscoveryPublish -> KeyResetOwed(
                owed.attestationB64, owed.keyGeneration, owed.signedAtMs, owed.newSignaturePubkeyB64,
                discoveryPublished = true, deviceGroupReset = owed.deviceGroupReset,
            )
            KeyResetStep.DeviceGroupReset -> KeyResetOwed(
                owed.attestationB64, owed.keyGeneration, owed.signedAtMs, owed.newSignaturePubkeyB64,
                discoveryPublished = owed.discoveryPublished, deviceGroupReset = true,
            )
            KeyResetStep.Done -> owed
        }
        save(accountIdB64, next)
    }

    public companion object {
        public const val PREFS_NAME: String = "lakko-key-reset-owed"
        private const val KEY_PREFIX: String = "key-reset-v1:"
        private val json = Json { ignoreUnknownKeys = true }

        public fun onDevice(context: Context): KeyResetOwedStore =
            KeyResetOwedStore(SharedPrefsMarkerStorage(context, PREFS_NAME))
    }
}

private const val HTTP_BAD_REQUEST: Int = 400
private const val HTTP_CONFLICT: Int = 409
private const val HTTP_TOO_MANY: Int = 429
