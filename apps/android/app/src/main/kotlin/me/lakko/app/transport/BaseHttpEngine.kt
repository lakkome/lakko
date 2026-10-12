// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.transport

import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.engine.okhttp.OkHttp
import io.ktor.client.engine.okhttp.OkHttpConfig
import java.util.ServiceLoader

/**
 * Supplies the engine that stands where OkHttp would. The app ships no
 * implementation; the unit-test source set registers one (through
 * `META-INF/services`) that refuses every request, so a test that builds a
 * client without handing it an engine cannot reach a server.
 */
public interface BaseHttpEngineProvider {
    public fun create(): HttpClientEngine
}

/**
 * The ONE place this app creates the engine that opens sockets. Every
 * default engine ([lakkoHttpEngine], the anonymous-token engine, the SFU
 * signaling socket, the call-state probe) is built on [create], so replacing
 * it replaces the network for the whole process.
 */
public object BaseHttpEngine {
    private val provider: BaseHttpEngineProvider? by lazy {
        try {
            ServiceLoader
                .load(BaseHttpEngineProvider::class.java, BaseHttpEngineProvider::class.java.classLoader)
                .firstOrNull()
        } catch (_: Throwable) {
            null
        }
    }

    /** OkHttp, configured by [configure]; or the registered provider's
     *  engine, which ignores the configuration. */
    public fun create(configure: OkHttpConfig.() -> Unit = {}): HttpClientEngine =
        provider?.create() ?: OkHttp.create(configure)
}
