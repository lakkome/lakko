// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.transport

import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.engine.HttpClientEngineBase
import io.ktor.client.engine.HttpClientEngineCapability
import io.ktor.client.engine.HttpClientEngineConfig
import io.ktor.client.engine.callContext
import io.ktor.client.engine.mergeHeaders
import io.ktor.client.request.HttpRequestData
import io.ktor.client.request.HttpResponseData
import io.ktor.http.HeadersBuilder
import io.ktor.http.HttpProtocolVersion
import io.ktor.http.HttpStatusCode
import io.ktor.http.content.OutgoingContent
import io.ktor.util.date.GMTDate
import io.ktor.utils.io.ByteReadChannel
import io.ktor.utils.io.InternalAPI
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.lakko_ffi_uniffi.FfiHeader
import uniffi.lakko_ffi_uniffi.tunneledHttpRequest

/**
 * ADR-0026 Phase 4 — the native HTTP routing configuration.
 *
 * When **armed**, a Lakko REST call routes through the best obfuscated
 * transport (the Rust `tunneledHttpRequest` FFI: verify the signed bootstrap
 * config against a pinned trust anchor, dial the transports fail-closed, run one
 * HTTP/1.1 request over the winning tunnel). When **disarmed** (the default),
 * the call takes the direct OkHttp path unchanged — there is no behaviour change
 * until a deployment arms it.
 *
 * Not a `data class` on purpose: the `ByteArray` members would give a misleading
 * structural `equals`/`hashCode`. This is a config holder, not a value key.
 */
public class AntiCensorshipConfig(
    /** Whether to route through the obfuscated tunnel. */
    public val armed: Boolean,
    /** The signed bootstrap-config JSON bytes (opaque; verified inside the FFI). */
    public val bootstrapConfigJson: ByteArray,
    /**
     * The 32-byte Ed25519 verifying key the config's signature is checked
     * against (build-time pinned).
     */
    public val trustAnchor: ByteArray,
    /** Dial deadline in milliseconds. */
    public val deadlineMs: ULong = 30_000uL,
) {
    public companion object {
        /** The default: direct path, no tunnel. Identical to today. */
        public val DISARMED: AntiCensorshipConfig =
            AntiCensorshipConfig(armed = false, bootstrapConfigJson = ByteArray(0), trustAnchor = ByteArray(0))
    }
}

/** Raised when a request cannot be routed through the tunnel. */
public class AntiCensorshipException(message: String) : Exception(message)

/**
 * A Ktor [HttpClientEngine] that routes every request through the obfuscated
 * tunnel when [config] is armed, otherwise delegates verbatim to [delegate]
 * (the production OkHttp engine). This is the single chokepoint every Ktor
 * request passes through, so adopting it leaves every request helper in
 * [me.lakko.app.discovery.DiscoveryClient] untouched — the Android analog of
 * the iOS `AntiCensorshipTransport.perform` seam.
 *
 * `DiscoveryClient` is the first client to adopt the seam; the rest migrate to
 * the same engine in a follow-up. The on-the-wire tunnel only does anything
 * against a deployed `services/obfs-proxy` (ADR-0026 Phase 5).
 */
public class AntiCensorshipEngine(
    private val delegate: HttpClientEngine,
    private val tunnelConfig: AntiCensorshipConfig,
) : HttpClientEngineBase("lakko-anti-censorship") {

    override val config: HttpClientEngineConfig = HttpClientEngineConfig()

    override val supportedCapabilities: Set<HttpClientEngineCapability<*>>
        get() = delegate.supportedCapabilities

    @OptIn(InternalAPI::class)
    override suspend fun execute(data: HttpRequestData): HttpResponseData {
        if (!tunnelConfig.armed) {
            return delegate.execute(data)
        }
        val callContext = callContext()
        val url = data.url
        val host = url.host
        // Ktor's `Url.port` resolves the protocol default when unspecified.
        val port = url.port.toUShort()
        val method = data.method.value
        val path = buildString {
            append(url.encodedPath.ifEmpty { "/" })
            val query = url.encodedQuery
            if (query.isNotEmpty()) {
                append('?')
                append(query)
            }
        }
        // `mergeHeaders` folds in the body's Content-Type / Content-Length the
        // same way a real engine would, so the tunnelled request carries them.
        val headers = ArrayList<FfiHeader>()
        mergeHeaders(data.headers, data.body) { key, value ->
            headers.add(FfiHeader(key, value))
        }
        val body = readBodyBytes(data.body)
        val nowSecs = (System.currentTimeMillis() / 1000L).toULong()

        // `tunneledHttpRequest` drives a private tokio runtime to completion —
        // run it off the calling dispatcher so it never blocks a UI thread.
        val resp = withContext(Dispatchers.IO) {
            tunneledHttpRequest(
                configJson = tunnelConfig.bootstrapConfigJson,
                trustAnchor = tunnelConfig.trustAnchor,
                nowSecs = nowSecs,
                endpointHost = host,
                endpointPort = port,
                deadlineMs = tunnelConfig.deadlineMs,
                method = method,
                path = path,
                host = host,
                headers = headers,
                body = body,
            )
        }

        val responseHeaders = HeadersBuilder().apply {
            for (header in resp.headers) {
                append(header.name, header.value)
            }
        }.build()

        return HttpResponseData(
            statusCode = HttpStatusCode.fromValue(resp.status.toInt()),
            requestTime = GMTDate(),
            headers = responseHeaders,
            version = HttpProtocolVersion.HTTP_1_1,
            body = ByteReadChannel(resp.body),
            callContext = callContext,
        )
    }

    /**
     * Read the outgoing body into bytes. The Discovery surface only ever sends
     * a JSON body (a `TextContent`, which is a `ByteArrayContent`) or no body,
     * so those two cases cover it; anything else fails closed rather than
     * silently sending an empty body.
     */
    private fun readBodyBytes(content: OutgoingContent): ByteArray = when (content) {
        is OutgoingContent.NoContent -> ByteArray(0)
        is OutgoingContent.ByteArrayContent -> content.bytes()
        else -> throw AntiCensorshipException(
            "unsupported request body type for tunnel: ${content::class.simpleName}",
        )
    }

    override fun close() {
        delegate.close()
        super.close()
    }
}
