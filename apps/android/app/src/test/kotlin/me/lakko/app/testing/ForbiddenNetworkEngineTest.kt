// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.testing

import io.ktor.client.HttpClient
import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.client.request.get
import io.ktor.client.statement.bodyAsText
import kotlinx.coroutines.test.runTest
import me.lakko.app.anon.AnonymousHttp
import me.lakko.app.transport.BaseHttpEngine
import me.lakko.app.transport.lakkoHttpEngine
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * Test hygiene: no unit test reaches a server. The guard itself is tested
 * here: every engine the app builds by default is the refusing one in this
 * JVM, a request through it fails and is recorded, and a test that hands a
 * client its own engine is not touched.
 */
class ForbiddenNetworkEngineTest {

    private suspend fun refused(client: HttpClient, url: String): String {
        ForbiddenNetworkEngine.logging = false
        try {
            client.get(url)
            fail("the request must not be sent")
            return ""
        } catch (cause: UnstubbedNetworkRequest) {
            return cause.message.orEmpty()
        } finally {
            ForbiddenNetworkEngine.logging = true
            client.close()
        }
    }

    @Test
    fun `the app's base engine is the refusing one under test`() {
        val engine = BaseHttpEngine.create()
        try {
            assertTrue("got ${engine::class.java.name}", engine is ForbiddenNetworkEngine)
        } finally {
            engine.close()
        }
    }

    @Test
    fun `a request through the default engine fails and is recorded, without its query`() = runTest {
        val before = ForbiddenNetworkEngine.refused.size
        val message = refused(HttpClient(lakkoHttpEngine()), "https://test.lakko.me/native/ds/group/abc/messages?token=secret")
        assertTrue(message, message.contains("GET https://test.lakko.me/native/ds/group/abc/messages"))
        val recorded = ForbiddenNetworkEngine.refused.drop(before)
        assertEquals(listOf("GET https://test.lakko.me/native/ds/group/abc/messages"), recorded)
        assertTrue("a query string can carry a token", recorded.none { it.contains("secret") })
    }

    @Test
    fun `the anonymous-token engine is refused too`() = runTest {
        val message = refused(HttpClient(AnonymousHttp.engine), "https://test.lakko.me/native/auth/tokens")
        assertTrue(message, message.contains("GET https://test.lakko.me/native/auth/tokens"))
    }

    @Test
    fun `a test that installs its own engine is answered by it`() = runTest {
        val before = ForbiddenNetworkEngine.refused.size
        val client = HttpClient(MockEngine { respond("stubbed") })
        try {
            assertEquals("stubbed", client.get("https://test.lakko.me/anything").bodyAsText())
        } finally {
            client.close()
        }
        assertEquals(before, ForbiddenNetworkEngine.refused.size)
    }

    @Test
    fun `no production code builds a socket-opening engine of its own`() {
        val root = java.io.File("src/main/kotlin")
        assertTrue("run from the app module: ${root.absolutePath}", root.isDirectory)
        // An engine built any of these ways would not be the guarded one: a
        // direct OkHttp engine, or a client with no engine at all (Ktor then
        // picks OkHttp itself).
        val direct = Regex("""OkHttp\.create|HttpClient\(OkHttp|HttpClient\(\s*\)|HttpClient\s*\{|OkHttpClient\(|OkHttpClient\.Builder""")
        val offenders = root.walkTopDown()
            .filter { it.isFile && it.extension == "kt" && it.name != "BaseHttpEngine.kt" }
            .flatMap { file ->
                file.readLines().withIndex()
                    .filter { (_, line) ->
                        val code = line.trim()
                        !code.startsWith("//") && !code.startsWith("*") && !code.startsWith("/*") && direct.containsMatchIn(code)
                    }
                    .map { (i, _) -> "${file.relativeTo(root).path}:${i + 1}" }
            }
            .toList()
        assertEquals("build the engine with BaseHttpEngine.create()", emptyList<String>(), offenders)
    }
}
