// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.transport

import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.engine.HttpClientEngineBase
import io.ktor.client.engine.HttpClientEngineCapability
import io.ktor.client.engine.HttpClientEngineConfig
import io.ktor.client.engine.okhttp.OkHttp
import io.ktor.client.request.HttpRequestData
import io.ktor.client.request.HttpResponseData
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.content.OutgoingContent
import io.ktor.http.headers
import io.ktor.utils.io.ByteReadChannel
import io.ktor.utils.io.InternalAPI
import io.ktor.utils.io.readRemaining
import kotlinx.io.readByteArray
import me.lakko.app.anon.errorCodeOf
import me.lakko.app.auth.SessionRefusal
import me.lakko.app.auth.SessionTokenProvider
import me.lakko.app.auth.SessionTokens
import me.lakko.app.auth.sessionRefusalOf

/**
 * ADR-0065 section 3.3 — the one place every bearer request passes through.
 *
 * Every REST client of this app builds its Ktor `HttpClient` over
 * [lakkoHttpEngine], which is this engine around the anti-censorship one
 * around OkHttp. For a request whose `Authorization: Bearer` is an access
 * token of the account the [SessionTokenProvider] serves, it
 *
 *   1. replaces the token with the provider's CURRENT one (refreshing first
 *      when 120 s or less remain), so a token a client captured at its
 *      session landing is never the stale one on the wire;
 *   2. on a plain `401` refreshes once and retries the request once, with
 *      the new token (only when the body can be sent again: no body, or
 *      bytes already in memory);
 *   3. on `401 key-generation-superseded`, from the first send or the
 *      retry, reports it to the provider - the account's keys were reset
 *      elsewhere and the device signs out (contract section 7) - and hands
 *      the response back unchanged.
 *
 * A request with no bearer, a refresh credential as bearer, or a bearer of
 * another account goes through untouched; the provider never substitutes a
 * token it does not manage. The DS speaks proofs, not bearers, so it is
 * never touched here.
 */
@OptIn(InternalAPI::class)
public class SessionBearerEngine(
    private val delegate: HttpClientEngine,
) : HttpClientEngineBase("lakko-session-bearer") {

    override val config: HttpClientEngineConfig = HttpClientEngineConfig()

    override val supportedCapabilities: Set<HttpClientEngineCapability<*>>
        get() = delegate.supportedCapabilities

    override suspend fun execute(data: HttpRequestData): HttpResponseData {
        val provider = SessionTokens.provider ?: return delegate.execute(data)
        val bearer = bearerOf(data) ?: return delegate.execute(data)
        if (!provider.manages(bearer)) return delegate.execute(data)

        val token = provider.current() ?: bearer
        val first = delegate.execute(if (token == bearer) data else withBearer(data, token))
        if (first.statusCode != HttpStatusCode.Unauthorized) return first

        val (body, firstAgain) = drainBody(first)
        when (sessionRefusalOf(first.statusCode.value, errorCodeOf(String(body, Charsets.UTF_8)))) {
            SessionRefusal.KeysReset -> {
                SessionTokens.reportSuperseded()
                return firstAgain
            }
            SessionRefusal.RefreshInvalid, SessionRefusal.Unauthorized, null -> Unit
        }
        if (!canResend(data.body)) return firstAgain
        val retryToken = provider.afterUnauthorized(token) ?: return firstAgain
        val second = delegate.execute(withBearer(data, retryToken))
        if (second.statusCode != HttpStatusCode.Unauthorized) return second
        val (secondBody, secondAgain) = drainBody(second)
        if (sessionRefusalOf(second.statusCode.value, errorCodeOf(String(secondBody, Charsets.UTF_8))) ==
            SessionRefusal.KeysReset
        ) {
            SessionTokens.reportSuperseded()
        }
        return secondAgain
    }

    override fun close() {
        delegate.close()
        super.close()
    }

    private fun bearerOf(data: HttpRequestData): String? {
        val value = data.headers[HttpHeaders.Authorization] ?: return null
        if (!value.startsWith(BEARER_PREFIX)) return null
        return value.removePrefix(BEARER_PREFIX).trim().takeIf { it.isNotEmpty() }
    }

    /** The same request with its bearer replaced. */
    @OptIn(InternalAPI::class)
    private fun withBearer(data: HttpRequestData, token: String): HttpRequestData {
        val replaced = headers {
            data.headers.forEach { name, values ->
                if (!name.equals(HttpHeaders.Authorization, ignoreCase = true)) appendAll(name, values)
            }
            append(HttpHeaders.Authorization, BEARER_PREFIX + token)
        }
        return HttpRequestData(
            url = data.url,
            method = data.method,
            headers = replaced,
            body = data.body,
            executionContext = data.executionContext,
            attributes = data.attributes,
        )
    }

    /** Only a body that is still in memory can go out a second time. */
    private fun canResend(body: OutgoingContent): Boolean =
        body is OutgoingContent.NoContent || body is OutgoingContent.ByteArrayContent

    /**
     * Read a `401` body (bounded: it is one small JSON object) and return it
     * with a response that still carries it, since reading consumed the
     * channel. A body that is not a channel is left as it is and classified
     * as a plain `401`.
     */
    @OptIn(InternalAPI::class)
    private suspend fun drainBody(response: HttpResponseData): Pair<ByteArray, HttpResponseData> {
        val channel = response.body as? ByteReadChannel ?: return ByteArray(0) to response
        val bytes = try {
            channel.readRemaining(MAX_ERROR_BODY_BYTES).readByteArray()
        } catch (_: Throwable) {
            ByteArray(0)
        }
        val again = HttpResponseData(
            statusCode = response.statusCode,
            requestTime = response.requestTime,
            headers = response.headers,
            version = response.version,
            body = ByteReadChannel(bytes),
            callContext = response.callContext,
        )
        return bytes to again
    }

    private companion object {
        const val BEARER_PREFIX: String = "Bearer "
        const val MAX_ERROR_BODY_BYTES: Long = 4096L
    }
}

/**
 * The engine every REST client of the app builds its `HttpClient` over: the
 * session-bearer layer (ADR-0065) around the anti-censorship routing
 * (ADR-0026) around OkHttp.
 */
public fun lakkoHttpEngine(): HttpClientEngine =
    SessionBearerEngine(AntiCensorshipEngine(OkHttp.create {}))
