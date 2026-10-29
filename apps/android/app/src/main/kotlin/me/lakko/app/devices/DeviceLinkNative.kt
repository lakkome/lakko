// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import android.content.Context
import me.lakko.app.auth.AuthClient

/*
 * ADR-0049 section 2 — the production bindings of the device-link ports: the
 * auth routes over [AuthClient], the core's SPAKE2 halves, and the answer
 * limit in the app's shared preferences.
 */

/** [DeviceLinkApi] over a short-lived [AuthClient] per call. The bearer routes
 *  read the session token NOW; with none they throw. */
public class AuthClientDeviceLinkApi(
    private val clientFactory: () -> AuthClient = { AuthClient() },
    private val sessionToken: () -> String? = { null },
    /** The live fresh-auth proof of the device that lets the new one in (UX
     *  17.4), read per call; `null` when none is live, and then nothing is
     *  sent for the two gated calls. */
    private val freshGrant: () -> String? = { null },
    /** Told when the server refused the proof (`403 fresh-auth-required`). */
    private val onProofRefused: () -> Unit = {},
) : DeviceLinkApi {
    private fun grant(): String =
        freshGrant()?.takeIf { it.isNotEmpty() } ?: throw me.lakko.app.auth.FreshAuthRequired()

    /** A `403 fresh-auth-required` as the one typed refusal the flow keeps
     *  the request across. */
    private suspend fun <T> gated(call: suspend (String) -> T): T {
        val g = grant()
        return try {
            call(g)
        } catch (cause: me.lakko.app.auth.AuthHttpError) {
            if (me.lakko.app.auth.isFreshAuthRequired(cause)) {
                onProofRefused()
                throw me.lakko.app.auth.FreshAuthRequired()
            }
            throw cause
        }
    }

    private fun bearer(): String =
        sessionToken()?.takeIf { it.isNotEmpty() } ?: throw IllegalStateException("no session")

    override suspend fun createRequest(handle: ByteArray): ByteArray =
        clientFactory().use { it.deviceRequestCreate(handle) }

    override suspend fun status(requestId: ByteArray): DeviceRequestStatus =
        clientFactory().use { it.deviceRequestStatus(requestId) }

    override suspend fun postPayload(requestId: ByteArray, pakeA: ByteArray, sealed: ByteArray, confirmA: ByteArray) {
        clientFactory().use { it.deviceRequestPayload(requestId, pakeA, sealed, confirmA) }
    }

    override suspend fun list(): PendingDeviceRequest? =
        clientFactory().use { it.deviceRequestsList(bearer()) }

    override suspend fun answer(requestId: ByteArray, pakeB: ByteArray): DeviceLinkAnswer =
        gated { g -> clientFactory().use { it.deviceRequestAnswer(bearer(), requestId, pakeB, g) } }

    override suspend fun postResult(requestId: ByteArray, accepted: Boolean, confirmB: ByteArray?) {
        gated { g -> clientFactory().use { it.deviceRequestResult(bearer(), requestId, accepted, confirmB, g) } }
    }

    override suspend fun decline(requestId: ByteArray) {
        clientFactory().use { it.deviceRequestDecline(bearer(), requestId) }
    }
}

/** The core's `DeviceLinkInitiator` as a [DeviceLinkInitiatorPort]. */
public class FfiDeviceLinkInitiator(requestId: ByteArray, loginHandle: ByteArray) : DeviceLinkInitiatorPort {
    private val inner = uniffi.lakko_ffi_uniffi.DeviceLinkInitiator(requestId, loginHandle)

    override fun finish(pakeB: ByteArray, typedCode: String, pairingPayload: ByteArray): SealedLink =
        try {
            inner.finish(pakeB, typedCode, pairingPayload).let { SealedLink(it.pakeA, it.sealedPayload, it.confirmA) }
        } catch (e: uniffi.lakko_ffi_uniffi.FfiException) {
            throw mapDeviceLinkError(e)
        }

    override fun verifyConfirmation(confirmB: ByteArray) {
        try {
            inner.verifyConfirmation(confirmB)
        } catch (e: uniffi.lakko_ffi_uniffi.FfiException) {
            throw mapDeviceLinkError(e)
        }
    }
}

/** The core's `DeviceLinkResponder` as a [DeviceLinkResponderPort]. */
public class FfiDeviceLinkResponder(requestId: ByteArray, ownLoginHandle: ByteArray) : DeviceLinkResponderPort {
    private val inner = uniffi.lakko_ffi_uniffi.DeviceLinkResponder(requestId, ownLoginHandle)

    override val code: String = inner.code()

    override val pakeMessage: ByteArray = inner.pakeMessage()

    override fun accept(pakeA: ByteArray, sealedPayload: ByteArray, confirmA: ByteArray): AcceptedLink =
        try {
            inner.accept(pakeA, sealedPayload, confirmA).let { AcceptedLink(it.pairingPayload, it.confirmB) }
        } catch (e: uniffi.lakko_ffi_uniffi.FfiException) {
            throw mapDeviceLinkError(e)
        }
}

/** The answer limit of [accountIdB64] on this device, persisted in shared
 *  preferences (a list of answer timestamps; not secret). Kept per account,
 *  like the web client's answer log: the times record when this account
 *  approved sign-ins, so a second account on the phone neither inherits nor
 *  spends them, and forget device removes them ([forgetDeviceLinkAnswers]). */
public fun deviceLinkAnswerLimit(context: Context, accountIdB64: String): DeviceLinkAnswerLimit {
    val prefs = context.applicationContext.getSharedPreferences(ANSWER_LIMIT_PREFS, Context.MODE_PRIVATE)
    val key = answerLimitKey(accountIdB64)
    return DeviceLinkAnswerLimit(
        load = { decodeAnswerTimes(prefs.getString(key, null)) },
        save = { times ->
            me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
                prefs.edit().putString(key, encodeAnswerTimes(times)).apply()
            }
        },
        enforced = !me.lakko.app.BuildConfig.DEBUG,
    )
}

/** Forget device: drop [accountIdB64]'s answer times and the retired
 *  device-wide row of builds before 2026-09-30. Other accounts' stay. */
public fun forgetDeviceLinkAnswers(context: Context, accountIdB64: String) {
    context.applicationContext.getSharedPreferences(ANSWER_LIMIT_PREFS, Context.MODE_PRIVATE).edit()
        .remove(answerLimitKey(accountIdB64))
        .remove(RETIRED_ANSWER_LIMIT_KEY)
        .apply()
}

/** `1,2,3` -> times; malformed entries skipped. */
public fun decodeAnswerTimes(text: String?): List<Long> =
    text.orEmpty().split(',').mapNotNull { it.trim().toLongOrNull() }

/** Times -> `1,2,3`. */
public fun encodeAnswerTimes(times: List<Long>): String = times.joinToString(",")

private fun answerLimitKey(accountIdB64: String): String =
    ANSWER_LIMIT_KEY_PREFIX + me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)

private const val ANSWER_LIMIT_PREFS = "lakko-device-link"
private const val ANSWER_LIMIT_KEY_PREFIX = "answered-at-v2:"

/** The device-wide row of builds before 2026-09-30. Never read. */
private const val RETIRED_ANSWER_LIMIT_KEY = "answered-at-v1"
