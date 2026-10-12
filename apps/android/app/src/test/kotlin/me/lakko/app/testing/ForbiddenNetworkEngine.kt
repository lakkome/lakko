// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.testing

import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.engine.HttpClientEngineBase
import io.ktor.client.engine.HttpClientEngineCapability
import io.ktor.client.engine.HttpClientEngineConfig
import io.ktor.client.plugins.HttpTimeoutCapability
import io.ktor.client.plugins.websocket.WebSocketCapability
import io.ktor.client.request.HttpRequestData
import io.ktor.client.request.HttpResponseData
import io.ktor.utils.io.InternalAPI
import java.io.File
import java.io.IOException
import me.lakko.app.transport.BaseHttpEngineProvider

/**
 * Test hygiene: NO unit test reaches a server.
 *
 * Every default engine of the app is built on
 * [me.lakko.app.transport.BaseHttpEngine], which looks this provider up
 * through `META-INF/services`. So a client a test builds WITHOUT handing it
 * an engine (a `ChatRepository` over its default factories, a default
 * `DsClient()`) gets this engine instead of OkHttp. A test that needs
 * requests answered passes its own engine (a `MockEngine`) and never comes
 * here.
 *
 * A refused request does two things:
 *
 *  - it fails as a transport error ([UnstubbedNetworkRequest], an
 *    `IOException`), which is what the code under test would see with no
 *    network; and
 *  - it is written, with the stack of the call, to the file named by the
 *    system property `lakko.test.networkLog`. The Gradle unit-test task
 *    fails when that file is not empty, so a request the code under test
 *    swallowed still fails the run and is named.
 */
public class ForbiddenNetworkEngineProvider : BaseHttpEngineProvider {
    override fun create(): HttpClientEngine = ForbiddenNetworkEngine()
}

/** What a refused request throws. */
public class UnstubbedNetworkRequest(summary: String) :
    IOException("a unit test reached the network without a stubbed engine: $summary")

public class ForbiddenNetworkEngine : HttpClientEngineBase("lakko-no-network-in-unit-tests") {
    override val config: HttpClientEngineConfig = HttpClientEngineConfig()

    // Declared so a client with timeouts or WebSockets installed reaches
    // [execute] (and is recorded) instead of failing on a missing capability.
    override val supportedCapabilities: Set<HttpClientEngineCapability<*>> =
        setOf(HttpTimeoutCapability, WebSocketCapability)

    /** Where this engine was built: usually the frame that names the test. */
    private val created = Throwable("engine created here")

    @InternalAPI
    override suspend fun execute(data: HttpRequestData): HttpResponseData {
        // Method, host and path only: a query string can carry a token.
        val summary = "${data.method.value} ${data.url.protocol.name}://${data.url.host}${data.url.encodedPath}"
        record(summary, created)
        throw UnstubbedNetworkRequest(summary)
    }

    public companion object {
        public const val PROPERTY: String = "lakko.test.networkLog"
        private val LOCK = Any()

        /** The requests refused in this JVM, for a test of the guard. */
        public val refused: MutableList<String> = java.util.concurrent.CopyOnWriteArrayList()

        /** While `false` a refusal is not written to the file (the guard's
         *  own test, which provokes one on purpose). */
        @Volatile
        public var logging: Boolean = true

        private fun record(summary: String, created: Throwable) {
            refused += summary
            if (!logging) return
            val path = System.getProperty(PROPERTY) ?: return
            val entry = buildString {
                append("---- unstubbed network request: ").append(summary).append('\n')
                append("thread ").append(Thread.currentThread().name).append(" at ")
                append(java.time.Instant.now()).append('\n')
                append(Throwable("call site").stackTraceToString()).append('\n')
                append(created.stackTraceToString()).append('\n')
            }
            synchronized(LOCK) {
                try {
                    File(path).appendText(entry)
                } catch (_: Throwable) {
                    // The run still fails each refused request as an IOException.
                }
            }
        }
    }
}
