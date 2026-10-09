// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
import kotlinx.coroutines.CancellationException
import uniffi.lakko_ffi_uniffi.Identity

/*
 * ADR-0066 section 0.4 — credentials name a device.
 *
 * A session's two credentials carry a 32-byte device id: this device's
 * conversation leaf signature key, which is also the id of its discovery
 * directory row. The client asserts it at every session landing
 * (`device_id_b64`); a session that landed without one is unbound, cannot
 * publish a KeyPackage and cannot be revoked per device, and is bound once
 * through `POST /auth/session/bind-device`.
 *
 * On Android the device seed is stored per account ([DeviceIdentityStore]),
 * so the id can be sent AT the landing only when the account is known before
 * the request: a signup (the client generates the account id), and any
 * sign-in on a device that already holds a session record for the typed
 * name. Every other landing (a fresh device signing in by name, a
 * discoverable passkey, an authenticator code, another-device redeem) learns
 * its account from the response and binds right after it, before the session
 * is installed. The decisions are pure and host-tested; the seed read and the
 * key derivation need the device.
 */

/** What a landing sends as `device_id_b64`. */
public sealed interface LandingDevice {
    /** The account is known beforehand: send this device's id for it. */
    public class Named(public val accountIdB64: String, public val deviceId: ByteArray) : LandingDevice

    /** The account is not known before the response: send nothing, then
     *  bind ([deviceBindingActionFor] answers [DeviceBindingAction.Bind]). */
    public data object BindAfter : LandingDevice
}

/**
 * The pure rule of what a landing sends. [candidateAccountIdB64] is the
 * account this device expects the landing to be for (`null` when it cannot
 * know); [deviceIdOf] reads or creates this device's id for an account and
 * answers `null` when it cannot.
 */
public fun landingDeviceFor(
    candidateAccountIdB64: String?,
    deviceIdOf: (accountIdB64: String) -> ByteArray?,
): LandingDevice {
    val account = candidateAccountIdB64?.takeIf { it.isNotBlank() } ?: return LandingDevice.BindAfter
    val id = deviceIdOf(account)?.takeIf { it.size == DEVICE_ID_LEN && !it.all { b -> b == 0.toByte() } }
        ?: return LandingDevice.BindAfter
    return LandingDevice.Named(account, id)
}

/**
 * The account a sign-in BY NAME is expected to land: the one this device
 * holds a session record for, when the typed name is that record's name.
 * Compared trimmed, without a leading `@`, case-insensitively; a guess that
 * turns out wrong is caught after the landing ([DeviceBindingAction.Foreign]).
 */
public fun candidateAccountForName(typedUsername: String, cached: PersistedSession?): String? {
    val record = cached ?: return null
    val typed = canonicalNameGuess(typedUsername)
    if (typed.isEmpty() || typed != canonicalNameGuess(record.username)) return null
    return record.accountIdB64
}

private fun canonicalNameGuess(name: String): String = name.trim().removePrefix("@").lowercase()

/** What to do about the device a landed session names. */
public enum class DeviceBindingAction {
    /** The access token names this device: nothing to do. */
    Bound,

    /** The session names no device: `POST /auth/session/bind-device`. */
    Bind,

    /**
     * The session names ANOTHER device id (the landing guessed the account
     * wrong, or this device's seed was replaced under a kept session). It
     * cannot be re-bound (`409 device-already-bound`) and cannot publish
     * this device's KeyPackage: the session is dropped and the person signs
     * in again, which lands a session that names this device.
     */
    Foreign,

    /** Not a `v3` access token: nothing is decided on a string this client
     *  cannot read. */
    Unknown,
}

/** The pure rule: compare the device segment of [accessToken] with this
 *  device's id for the landed account. */
public fun deviceBindingActionFor(accessToken: String, ownDeviceId: ByteArray): DeviceBindingAction {
    val segment = deviceSegmentOfAccessToken(accessToken) ?: return DeviceBindingAction.Unknown
    return when (segment) {
        base64UrlEncodeNoPad(ownDeviceId) -> DeviceBindingAction.Bound
        UNBOUND_DEVICE_SEGMENT -> DeviceBindingAction.Bind
        else -> DeviceBindingAction.Foreign
    }
}

/** `409` from `bind-device`: the session already names another device. */
public const val DEVICE_ALREADY_BOUND: String = "device-already-bound"

/** What `POST /auth/session/bind-device` did. */
public sealed interface BindDeviceOutcome {
    /** The pair that names this device; the unbound one is dropped. */
    public class Bound(public val pair: SessionPair) : BindDeviceOutcome

    /** `409 device-already-bound`: as [DeviceBindingAction.Foreign]. */
    public data object AlreadyBoundElsewhere : BindDeviceOutcome

    /** `401 device-revoked`: this device id was removed from the account. */
    public data object Revoked : BindDeviceOutcome

    /** `401 key-generation-superseded`: the keys were reset elsewhere. */
    public data object Superseded : BindDeviceOutcome

    /** Anything else (offline, a limit, a 5xx, `refresh-invalid`): the
     *  session stays unbound for now and the next landing tries again. */
    public class Retry(public val cause: Throwable) : BindDeviceOutcome
}

/** Classify the result of the bind request; `null` [error] with [pair]. */
public fun bindDeviceOutcome(pair: SessionPair?, error: Throwable?): BindDeviceOutcome = when {
    error == null && pair != null -> BindDeviceOutcome.Bound(pair)
    error is AuthHttpError && error.status == HTTP_CONFLICT_STATUS && error.code == DEVICE_ALREADY_BOUND ->
        BindDeviceOutcome.AlreadyBoundElsewhere
    error is AuthHttpError && sessionRefusalOf(error.status, error.code) == SessionRefusal.DeviceRevoked ->
        BindDeviceOutcome.Revoked
    error is AuthHttpError && sessionRefusalOf(error.status, error.code) == SessionRefusal.KeysReset ->
        BindDeviceOutcome.Superseded
    else -> BindDeviceOutcome.Retry(error ?: IllegalStateException("bind-device answered no pair"))
}

/** This device's id for an account: its conversation leaf signature key. */
public fun interface DeviceIdSource {
    /** `null` when the seed cannot be read or the key cannot be derived. */
    public fun deviceIdFor(accountIdB64: String): ByteArray?
}

/**
 * The on-device [DeviceIdSource]: the Ed25519 public key of the per-account
 * device seed ([DeviceIdentityStore], created when absent), which is the
 * signature key of every leaf and KeyPackage this device publishes
 * ([SessionIdentities.deviceIdentity] binds the same seed).
 *
 * A wipe ([me.lakko.app.account.AccountLocalWipe]) deletes the seed, so a
 * device that is set up again mints a NEW id; that is what lets it publish
 * after its own directory row was deleted and tombstoned (ADR-0066 "T1 as
 * built" point 21).
 */
public class OnDeviceIdSource(private val context: Context) : DeviceIdSource {
    override fun deviceIdFor(accountIdB64: String): ByteArray? {
        val seed = try {
            DeviceIdentityStore.ensureDeviceIdentitySeed(context.applicationContext, accountIdB64)
        } catch (cause: Throwable) {
            android.util.Log.w("lakko-auth", "device id: the device seed is not readable: ${cause.javaClass.simpleName}")
            return null
        }
        return try {
            Identity.fromSeed(seed).use { it.signaturePublicKey() }
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            android.util.Log.w("lakko-auth", "device id: the key could not be derived: ${cause.javaClass.simpleName}")
            null
        } finally {
            seed.fill(0)
        }
    }
}

/** A device id is 32 bytes. */
public const val DEVICE_ID_LEN: Int = 32

private const val HTTP_CONFLICT_STATUS: Int = 409
