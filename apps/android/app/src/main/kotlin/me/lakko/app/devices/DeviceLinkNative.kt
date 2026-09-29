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
) : DeviceLinkApi {
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
        clientFactory().use { it.deviceRequestAnswer(bearer(), requestId, pakeB) }

    override suspend fun postResult(requestId: ByteArray, accepted: Boolean, confirmB: ByteArray?) {
        clientFactory().use { it.deviceRequestResult(bearer(), requestId, accepted, confirmB) }
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

/** The per-device answer limit, persisted in shared preferences (a list of
 *  answer timestamps; not secret). */
public fun deviceLinkAnswerLimit(context: Context): DeviceLinkAnswerLimit {
    val prefs = context.applicationContext.getSharedPreferences(ANSWER_LIMIT_PREFS, Context.MODE_PRIVATE)
    return DeviceLinkAnswerLimit(
        load = { decodeAnswerTimes(prefs.getString(ANSWER_LIMIT_KEY, null)) },
        save = { times -> prefs.edit().putString(ANSWER_LIMIT_KEY, encodeAnswerTimes(times)).apply() },
    )
}

/** `1,2,3` -> times; malformed entries skipped. */
public fun decodeAnswerTimes(text: String?): List<Long> =
    text.orEmpty().split(',').mapNotNull { it.trim().toLongOrNull() }

/** Times -> `1,2,3`. */
public fun encodeAnswerTimes(times: List<Long>): String = times.joinToString(",")

private const val ANSWER_LIMIT_PREFS = "lakko-device-link"
private const val ANSWER_LIMIT_KEY = "answered-at-v1"
