// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlin.coroutines.cancellation.CancellationException
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.discovery.DiscoveryHttpError
import me.lakko.app.discovery.DiscoveryNetworkError

/**
 * Run one bearer-gated discovery request (a route behind the server's
 * `require_session` layer: invite upload, invite revoke, ...) on a
 * short-lived client carrying the signed-in session token.
 *
 * The repository's shared [DiscoveryClient] carries no token on purpose, so
 * the session is never attached to its anonymous reads; a bearer route sent
 * through it is answered `401`. [sessionToken] is read by the CALLER at call
 * time, so a re-login's newer token is the one used. No session at all is a
 * loud [ChatError] before any request, never a silent downgrade.
 */
internal suspend fun <T> withSessionDiscovery(
    sessionToken: String?,
    what: String,
    clientFactory: () -> DiscoveryClient,
    block: suspend (DiscoveryClient) -> T,
): T {
    val token = sessionToken?.takeIf { it.isNotEmpty() }
        ?: throw ChatError("$what needs a signed-in session", IllegalStateException("no session token"))
    return clientFactory().use { client ->
        client.setSessionToken(token)
        block(client)
    }
}

/**
 * ADR-0028 OQ1 — upload a group-invite capability to the discovery ledger
 * under the signed-in session and answer its 6-byte `blob_id`.
 *
 * Every failure is thrown as a [ChatError] and mints no link: a server
 * refusal (a `401` for a missing or expired session, a `400`, a `429`) and an
 * unreachable ledger alike (the inline form that used to be shared then was
 * removed on 2026-10-19). [log] receives the status or the failure class,
 * never the capability.
 */
internal suspend fun uploadGroupInvite(
    sessionToken: String?,
    clientFactory: () -> DiscoveryClient,
    upload: suspend (DiscoveryClient) -> ByteArray,
    log: (String) -> Unit,
): ByteArray =
    withSessionDiscovery(sessionToken, "group invite upload", clientFactory) { client ->
        try {
            upload(client)
        } catch (e: DiscoveryNetworkError) {
            if (e.cause is CancellationException) throw e.cause as CancellationException
            log("group invite upload unreachable (${e.cause?.javaClass?.simpleName})")
            throw ChatError("group invite upload unreachable", e)
        } catch (e: DiscoveryHttpError) {
            log("group invite upload refused: HTTP ${e.status}")
            throw ChatError("group invite upload refused (HTTP ${e.status})", e)
        }
    }
