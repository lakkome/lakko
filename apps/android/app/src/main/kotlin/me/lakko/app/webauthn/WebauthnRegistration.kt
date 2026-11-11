// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.webauthn

import android.content.Context
import androidx.credentials.CreatePublicKeyCredentialRequest
import androidx.credentials.CreatePublicKeyCredentialResponse
import androidx.credentials.CredentialManager
import androidx.credentials.GetCredentialRequest
import androidx.credentials.GetPublicKeyCredentialOption
import androidx.credentials.PublicKeyCredential
import androidx.credentials.exceptions.CreateCredentialCancellationException
import androidx.credentials.exceptions.CreateCredentialException
import androidx.credentials.exceptions.GetCredentialCancellationException
import androidx.credentials.exceptions.GetCredentialException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject

/**
 * `WebauthnRegistration` — async wrapper around Jetpack
 * [CredentialManager] for the Phase 2a2e passkey flows.
 *
 * Mirror of:
 *
 *   * `apps/web/src/auth/webauthn-client.ts`'s
 *     `navigator.credentials.create` / `.get` calls
 *   * `apps/ios/Lakko/Webauthn/WebauthnRegistration.swift`'s
 *     `ASAuthorizationController` bridge
 *
 * The Credential Manager API takes / returns the WebAuthn JSON
 * directly (`requestJson` / `registrationResponseJson` /
 * `authenticationResponseJson`), so this helper is much thinner
 * than the iOS bridge — no manual base64url-conversion is
 * needed.
 *
 * ## Platform requirements
 *
 *   * `minSdk = 26` — Credential Manager backports to API 26,
 *     but the on-device platform passkey provider lands on
 *     API 28+ in practice. Lower API devices fall through to
 *     the Play Services credential provider.
 *   * The relying party must publish a Digital Asset Links
 *     `assetlinks.json` for the app's package signature at
 *     `https://<rp_id>/.well-known/assetlinks.json`. Mirror
 *     of the iOS `apple-app-site-association` requirement.
 *
 * ## Two flows
 *
 *   * [register] — drive [CreatePublicKeyCredentialRequest].
 *   * [authenticate] — drive [GetCredentialRequest] with a
 *     [GetPublicKeyCredentialOption]. Consumed by the UI via
 *     `WebauthnClient.loginWithPasskey` (login) and
 *     `WebauthnClient.assertAction` (ADR-0014 action binding
 *     for remove-device / delete-passkey).
 */
public class WebauthnRegistration {

    /** Lenient JSON parser — must accept whatever shape the
     *  Credential Manager hands back. */
    private val json = Json {
        ignoreUnknownKeys = true
        explicitNulls = false
    }

    /**
     * What one ceremony produced. [credential] is the response JSON as the
     * server may see it (the `prf` entry already removed, see
     * [serverSafeCredential]); [prfOutput] stays on this device.
     */
    public class Ceremony(
        public val credential: JsonElement,
        /** Raw credential id of the passkey that answered, when the
         *  response named one. */
        public val credentialId: ByteArray?,
        /** The 32-byte PRF output, or `null` when the provider returned
         *  none: the passkey then signs in only. */
        public val prfOutput: ByteArray?,
        /** What a registration reported about PRF support (see [PrfResult]). */
        public val prfEnabled: Boolean?,
    )

    private fun ceremonyOf(responseJson: String): Ceremony {
        val parsed = json.parseToJsonElement(responseJson)
        val prf = prfResultOf(parsed)
        return Ceremony(
            credential = serverSafeCredential(parsed),
            credentialId = credentialIdOf(parsed),
            prfOutput = prf.output,
            prfEnabled = prf.enabled,
        )
    }

    /** Sealed outcome of [register] / [authenticate] so the
     *  caller can branch cleanly on the cancellation path. */
    public sealed interface Outcome<out T> {
        public data class Success<T>(val value: T) : Outcome<T>
        public data object Cancelled : Outcome<Nothing>
        public data class Failed(val cause: Throwable) : Outcome<Nothing>
    }

    /**
     * Run the registration round.
     *
     * `optionsPublicKey` is the inner `publicKey` field of the
     * server's `CreationChallengeResponse` — re-serialised to a
     * JSON string in the WebAuthn level-2 shape Credential
     * Manager accepts. On success returns the parsed
     * `registrationResponseJson` ([Ceremony.credential]) ready to feed into
     * `WebauthnClient.finishRegistration`.
     *
     * With [prfSalt] the request also asks the new credential to evaluate
     * the PRF extension; the result is in [Ceremony.prfOutput] /
     * [Ceremony.prfEnabled].
     */
    public suspend fun register(
        context: Context,
        optionsPublicKey: JsonElement,
        prfSalt: ByteArray? = null,
    ): Outcome<Ceremony> {
        val options = if (prfSalt != null) withPrfExtension(optionsPublicKey, prfSalt) else optionsPublicKey
        val requestJson = json.encodeToString(JsonElement.serializer(), options)
        return try {
            val manager = CredentialManager.create(context)
            val request = CreatePublicKeyCredentialRequest(requestJson = requestJson)
            val response = manager.createCredential(context, request)
            val pk = response as? CreatePublicKeyCredentialResponse
                ?: return Outcome.Failed(
                    IllegalStateException("unexpected credential response type"),
                )
            Outcome.Success(ceremonyOf(pk.registrationResponseJson))
        } catch (_: CreateCredentialCancellationException) {
            Outcome.Cancelled
        } catch (e: CreateCredentialException) {
            Outcome.Failed(registrationFailure(e))
        } catch (e: Throwable) {
            Outcome.Failed(e)
        }
    }

    /**
     * Run the authentication round.
     *
     * `optionsPublicKey` is the inner `publicKey` field of the
     * server's `RequestChallengeResponse`. On success returns
     * the parsed `authenticationResponseJson` ([Ceremony.credential]) ready
     * to feed into `WebauthnClient.finishAuthentication`.
     *
     * With [prfSalt] the assertion also evaluates the PRF extension; with
     * [onlyCredentialIdB64] the sheet offers that one passkey only.
     */
    public suspend fun authenticate(
        context: Context,
        optionsPublicKey: JsonElement,
        prfSalt: ByteArray? = null,
        onlyCredentialIdB64: String? = null,
    ): Outcome<Ceremony> {
        var options = optionsPublicKey
        if (onlyCredentialIdB64 != null) options = onlyCredential(options, onlyCredentialIdB64)
        if (prfSalt != null) options = withPrfExtension(options, prfSalt)
        val requestJson = json.encodeToString(JsonElement.serializer(), options)
        return try {
            val manager = CredentialManager.create(context)
            val option = GetPublicKeyCredentialOption(requestJson = requestJson)
            val request = GetCredentialRequest(listOf(option))
            val response = manager.getCredential(context, request)
            val pk = response.credential as? PublicKeyCredential
                ?: return Outcome.Failed(
                    IllegalStateException("unexpected credential response type"),
                )
            Outcome.Success(ceremonyOf(pk.authenticationResponseJson))
        } catch (_: GetCredentialCancellationException) {
            Outcome.Cancelled
        } catch (e: GetCredentialException) {
            Outcome.Failed(e)
        } catch (e: Throwable) {
            Outcome.Failed(e)
        }
    }
}

/**
 * What a refused registration means. The provider refusing because it
 * already holds a passkey for this account (WebAuthn `InvalidStateError`,
 * the excluded-credential case) is [PasskeyAlreadyExistsError]: the device
 * can create passkeys, it just has one for this account (UX 10.2). Every
 * other refusal is returned as it was.
 */
public fun registrationFailure(e: Throwable): Throwable =
    if (
        e is androidx.credentials.exceptions.publickeycredential.CreatePublicKeyCredentialDomException &&
        e.domError is androidx.credentials.exceptions.domerrors.InvalidStateError
    ) {
        PasskeyAlreadyExistsError(e)
    } else {
        e
    }

/** Convenience: extract the inner `publicKey` field from a
 *  webauthn-rs envelope (`CreationChallengeResponse` /
 *  `RequestChallengeResponse`). Mirror of the inline
 *  `options.publicKey` access on the web side. Throws when the
 *  envelope is malformed — should never happen on a server-
 *  generated payload. */
internal fun JsonElement.publicKeyField(): JsonElement {
    val obj: JsonObject = jsonObject
    return obj["publicKey"] ?: throw IllegalArgumentException(
        "webauthn options envelope missing `publicKey` field",
    )
}
