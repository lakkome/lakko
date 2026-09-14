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
