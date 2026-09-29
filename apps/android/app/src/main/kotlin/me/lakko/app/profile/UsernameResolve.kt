// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import java.util.Base64
import kotlinx.coroutines.CancellationException

/**
 * ADR-0048 §3 — resolve a claimed username to the signature key discovery
 * serves for it, for [UsernameVerifier].
 *
 * The plain path the ADR specifies, and the one iOS
 * (`ProfileDirectory.discoveryResolve`) and web take: discovery `lookup` ->
 * `account_id` -> identity bundle -> signature public key. Nothing else.
 *
 * ## Why no pin gate
 *
 * The Android profile wave first routed this through the contacts
 * `resolveVerifiedRecipient`, which runs the G-4 supersession / pin gate. That
 * gate PINS a peer's account key the first time it sees it (TOFU), so merely
 * rendering a username in a chat decided which key a later contact request or
 * add-member would trust, and a refused changed key surfaced here as an
 * unexplained "unavailable". Verification is a comparison, not a trust
 * decision: the username is shown only when the served key EQUALS the key the
 * profile is already bound to (the MLS-authenticated sender key, C-68), so it
 * needs no pin, and must not write one. The gate stays on the paths that seal
 * to a key.
 *
 * ## Outcomes
 *
 *  - no account for the username, no bundle, or a bundle key that is not
 *    32 bytes -> [UsernameResolution.NotRegistered] (a definitive failure,
 *    cached by the verifier);
 *  - a lookup or fetch that throws -> [UsernameResolution.Unavailable]
 *    (nothing cached, retried later);
 *  - otherwise [UsernameResolution.Found] with the key in standard base64.
 *
 * @param lookupAccountId discovery lookup; `null` when unregistered.
 * @param fetchSignatureKey the bundle's signature key for an account, or
 *   `null` when there is none.
 */
public suspend fun resolveUsernameForVerification(
    username: String,
    lookupAccountId: suspend (String) -> ByteArray?,
    fetchSignatureKey: suspend (ByteArray) -> ByteArray?,
): UsernameResolution =
    try {
        val accountId = lookupAccountId(username)
        val key = accountId?.let { fetchSignatureKey(it) }
        when {
            key == null -> UsernameResolution.NotRegistered
            key.size != SIGNATURE_KEY_BYTES -> UsernameResolution.NotRegistered
            else -> UsernameResolution.Found(Base64.getEncoder().encodeToString(key))
        }
    } catch (cancel: CancellationException) {
        throw cancel
    } catch (error: Throwable) {
        UsernameResolution.Unavailable(error)
    }

private const val SIGNATURE_KEY_BYTES = 32

/**
 * ADR-0058 §5 — verify a claimed username by its discovery-signed certificate
 * instead of a public lookup (which a person outside Everyone no longer
 * answers).
 *
 * Found only when all three hold: the certificate verifies for the OPRF of
 * [username] and the claimed account ([accountIdHex]) under the pinned key
 * ([verifyBinding]); and that account's published signature key is the
 * sender's ([senderKeyB64], resolved at receipt, C-68) — without the last
 * check a member could copy a stranger's certificate and account id into
 * their own profile. A mismatch is [UsernameResolution.NotRegistered]; a
 * network failure or a missing key is [UsernameResolution.Unavailable] (no
 * conclusion, retried later).
 *
 * @param oprf the 64-byte OPRF output of the username (one lookup token).
 * @param verifyBinding the certificate check (`verifyUsernameBinding`), `true`
 *   when it accepts; `null` when no verifying key is available.
 */
public suspend fun verifyUsernameByCertificate(
    username: String,
    certificateHex: String,
    accountIdHex: String,
    senderKeyB64: String,
    oprf: suspend (String) -> ByteArray,
    verifyBinding: ((certificate: ByteArray, oprfHash: ByteArray, accountId: ByteArray) -> Boolean)?,
    fetchSignatureKey: suspend (ByteArray) -> ByteArray?,
): UsernameResolution {
    val certificate = hexBytesOrNull(certificateHex) ?: return UsernameResolution.NotRegistered
    val accountId = hexBytesOrNull(accountIdHex)?.takeIf { it.size == ACCOUNT_ID_BYTES }
        ?: return UsernameResolution.NotRegistered
    val binding = verifyBinding ?: return UsernameResolution.Unavailable(null)
    return try {
        val hash = oprf(username)
        if (!binding(certificate, hash, accountId)) return UsernameResolution.NotRegistered
        val published = fetchSignatureKey(accountId) ?: return UsernameResolution.NotRegistered
        if (Base64.getEncoder().encodeToString(published) != senderKeyB64) {
            UsernameResolution.NotRegistered
        } else {
            UsernameResolution.Found(senderKeyB64)
        }
    } catch (cancel: CancellationException) {
        throw cancel
    } catch (error: Throwable) {
        UsernameResolution.Unavailable(error)
    }
}

private fun hexBytesOrNull(hex: String): ByteArray? {
    if (hex.length % 2 != 0) return null
    val out = ByteArray(hex.length / 2)
    for (i in out.indices) {
        val hi = Character.digit(hex[2 * i], 16)
        val lo = Character.digit(hex[2 * i + 1], 16)
        if (hi < 0 || lo < 0) return null
        out[i] = ((hi shl 4) or lo).toByte()
    }
    return out
}
