// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.webauthn

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/*
 * The WebAuthn PRF extension over the JSON the Credential Manager carries.
 *
 * A passkey signs the person in; with PRF it can also carry the account's
 * keys. The PRF output for a fixed salt is a stable 32-byte secret per
 * credential that follows a synced passkey to every device the passkey
 * syncs to. The client seals the phrase entropy under it and stores the blob
 * server-side per credential (every passkey that can, UX 10.2).
 *
 * Android's `CreatePublicKeyCredentialRequest` and
 * `GetPublicKeyCredentialOption` take the WebAuthn request as a JSON string
 * and hand the response back as a JSON string, so the extension is requested
 * by adding `extensions.prf.eval.first` (base64url) to the request and read
 * from `clientExtensionResults.prf` of the response. Whether an output comes
 * back is up to the credential provider; when none does, the passkey signs
 * in only.
 *
 * The PRF output NEVER leaves this device: [serverSafeCredential] removes
 * the `prf` entry before the response is posted to the server.
 *
 * Everything here is pure JSON, so the host tests cover it.
 */

/** The PRF salt every Lakko client evaluates. Fixed by design: the output is
 *  already per-credential, and a per-account salt would have to be fetched
 *  before the sign-in that needs it. */
public val PASSKEY_PRF_SALT: ByteArray = "lakko/passkey-vault/prf/v1".toByteArray(Charsets.UTF_8)

/** The length of a usable PRF output. Anything else is treated as none. */
public const val PASSKEY_PRF_OUTPUT_LENGTH: Int = 32

private val B64URL_ENCODER = java.util.Base64.getUrlEncoder().withoutPadding()
private val B64URL_DECODER = java.util.Base64.getUrlDecoder()

internal fun base64UrlNoPad(bytes: ByteArray): String = B64URL_ENCODER.encodeToString(bytes)

/** base64url with or without padding; `null` when [text] is not base64url. */
internal fun base64UrlOrNull(text: String): ByteArray? = try {
    B64URL_DECODER.decode(text.trimEnd('='))
} catch (_: IllegalArgumentException) {
    null
}

/**
 * The request options [publicKey] (the inner `publicKey` object of the
 * server's start response) with the PRF evaluation for [salt] added. Other
 * extensions the server asked for are kept.
 */
public fun withPrfExtension(publicKey: JsonElement, salt: ByteArray = PASSKEY_PRF_SALT): JsonElement {
    val options = publicKey as? JsonObject ?: return publicKey
    val existing = options["extensions"] as? JsonObject
    val prf = buildJsonObject {
        put("eval", buildJsonObject { put("first", base64UrlNoPad(salt)) })
    }
    val extensions = JsonObject((existing ?: JsonObject(emptyMap())) + ("prf" to prf))
    return JsonObject(options + ("extensions" to extensions))
}

/**
 * The request options limited to one credential: the sheet then offers only
 * the passkey [credentialIdB64] (base64url, no padding).
 */
public fun onlyCredential(publicKey: JsonElement, credentialIdB64: String): JsonElement {
    val options = publicKey as? JsonObject ?: return publicKey
    val allow = JsonArray(
        listOf(
            buildJsonObject {
                put("type", "public-key")
                put("id", credentialIdB64)
            },
        ),
    )
    return JsonObject(options + ("allowCredentials" to allow))
}

/** What a ceremony says about PRF. */
public class PrfResult(
    /** The 32-byte `first` output, or `null`: not evaluated, not supported,
     *  or an output of another length. */
    public val output: ByteArray?,
    /** What a REGISTRATION reported: `true` when the new credential
     *  supports PRF (the output may only come at the next use), `false` when
     *  it does not, `null` when the provider said nothing. */
    public val enabled: Boolean?,
)

/** Read `clientExtensionResults.prf` of a registration or assertion
 *  response. Tolerant of every shape: anything unexpected reads as none. */
public fun prfResultOf(credential: JsonElement): PrfResult {
    val prf = ((credential as? JsonObject)?.get("clientExtensionResults") as? JsonObject)
        ?.get("prf") as? JsonObject
        ?: return PrfResult(output = null, enabled = null)
    val first = ((prf["results"] as? JsonObject)?.get("first") as? JsonPrimitive)
        ?.takeIf { it.isString }
        ?.content
    val bytes = first?.let(::base64UrlOrNull)
    val output = bytes?.takeIf { it.size == PASSKEY_PRF_OUTPUT_LENGTH }
    if (output == null) bytes?.fill(0)
    // A JSON boolean only: the string "true" is not an answer.
    val enabled = (prf["enabled"] as? JsonPrimitive)?.takeIf { !it.isString }?.booleanOrNull
    return PrfResult(output = output, enabled = enabled)
}

/**
 * The response as the server may see it: the same JSON with the `prf` entry
 * removed from `clientExtensionResults`. Applied to every response, whether
 * or not PRF was requested.
 */
public fun serverSafeCredential(credential: JsonElement): JsonElement {
    val response = credential as? JsonObject ?: return credential
    val results = response["clientExtensionResults"] as? JsonObject ?: return credential
    if ("prf" !in results) return credential
    return JsonObject(response + ("clientExtensionResults" to JsonObject(results - "prf")))
}

/** The credential id of a response (`rawId`, else `id`), or `null`. */
public fun credentialIdOf(credential: JsonElement): ByteArray? {
    val response = credential as? JsonObject ?: return null
    val text = sequenceOf("rawId", "id")
        .mapNotNull { (response[it] as? JsonPrimitive)?.takeIf { p -> p.isString }?.content }
        .firstOrNull()
        ?: return null
    return base64UrlOrNull(text)
}

// ---------------------------------------------------------------------------
// The rules around a passkey that restores keys (web: recovery/passkey-vault.ts)
// ---------------------------------------------------------------------------

/** What a passkey row says in Settings. */
public enum class PasskeyKeysLabel { RestoresKeys, SignInOnly }

/** The label for one passkey: it restores keys exactly when the server holds
 *  a blob for its credential id. [withKeys] is `GET /vault/passkeys`; `null`
 *  (not loaded, or the server keeps no such keys) reads as sign-in only. */
public fun passkeyKeysLabel(credentialIdB64: String, withKeys: List<String>?): PasskeyKeysLabel =
    if (withKeys != null && credentialIdB64 in withKeys) PasskeyKeysLabel.RestoresKeys else PasskeyKeysLabel.SignInOnly

/** What a passkey sign-in on a device without the account's keys found; it
 *  decides the first line of the restore prompt. */
public enum class PasskeyKeysOutcome {
    /** The blob opened and the phrase matched the published identity. */
    Restored,

    /** No PRF output, or no blob for this passkey: it signs in only. */
    NoKeys,

    /** A blob exists but did not open, or the phrase it gave is not the
     *  account's: nothing was changed. */
    Unusable,
}

/** The outcome for what the ceremony and the vault returned. */
public fun passkeyKeysOutcome(
    prfOutput: Boolean,
    blobStored: Boolean,
    opened: Boolean,
    matched: Boolean,
): PasskeyKeysOutcome = when {
    !prfOutput || !blobStored -> PasskeyKeysOutcome.NoKeys
    opened && matched -> PasskeyKeysOutcome.Restored
    else -> PasskeyKeysOutcome.Unusable
}

/** The restore prompt's rule for its passkey row: shown unless the account
 *  is known to have no passkey that carries keys ([passkeysWithKeys] is the
 *  count from `GET /vault/passkeys`; `null` while it is not known, which
 *  keeps the row). */
public fun restoreOffersPasskey(passkeysWithKeys: Int?): Boolean =
    passkeysWithKeys == null || passkeysWithKeys > 0
