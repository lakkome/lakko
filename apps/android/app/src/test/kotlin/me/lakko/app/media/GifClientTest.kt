// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.client.engine.mock.respondError
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import io.ktor.utils.io.ByteReadChannel
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0047 §4 / UX spec §5 — the GIF proxy client: the URLs it builds, the
 * contract it holds a response to, and the failure every state in the spec's
 * table maps from.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class GifClientTest {

    private val dev = GifClient.DEFAULT_BASE_URL
    private val prod = GifClient.PRODUCTION_BASE_URL

    @Test
    fun listUrlsAreBasePlusServicePath() {
        assertEquals("$dev/gifs/trending?offset=0", gifListUrl(dev, "   ", 0))
        assertEquals("$dev/gifs/search?q=cat&offset=24", gifListUrl(dev, " cat ", 24))
        // nginx strips its own /gifs prefix, so production nests it once more.
        assertEquals("https://api.lakko.me/gifs/gifs/search?q=happy%20cat&offset=0", gifListUrl(prod, "happy cat", 0))
        assertEquals("$dev/gifs/trending?offset=0", gifListUrl("$dev/", "", -5))
        assertEquals("$dev/gifs/search?q=a%26b%3Dc&offset=0", gifListUrl(dev, "a&b=c", 0))
    }

    @Test
    fun queryIsCappedAt100Characters() {
        val url = gifListUrl(dev, "x".repeat(150), 0)
        assertEquals("$dev/gifs/search?q=${"x".repeat(MAX_GIF_QUERY_CHARS)}&offset=0", url)
    }

    @Test
    fun onlyExactProxyMediaPathsBecomeUrls() {
        assertEquals("$prod/gifs/media/abc123/preview", gifMediaUrl(prod, "/gifs/media/abc123/preview"))
        assertEquals("$dev/gifs/media/Z9/full", gifMediaUrl(dev, "/gifs/media/Z9/full"))
        for (bad in listOf(
            "https://media.giphy.com/media/abc/giphy.gif",
            "//evil.example/gifs/media/abc/full",
            "/gifs/media/../../auth/full",
            "/gifs/media/abc/full?u=https://x",
            "/gifs/media/abc/original",
            "/gifs/media/ab-c/full",
            null,
        )) {
            assertNull("must refuse $bad", gifMediaUrl(dev, bad))
        }
    }

    @Test
    fun pageParsingDropsItemsOutsideTheContract() {
        val body = """
            {"items":[
              {"id":"ok1","preview_url":"/gifs/media/ok1/preview","full_url":"/gifs/media/ok1/full","width":480,"height":270},
              {"id":"bad-url","preview_url":"https://giphy.com/x","full_url":"/gifs/media/x/full","width":1,"height":1},
              {"id":"big","preview_url":"/gifs/media/big/preview","full_url":"/gifs/media/big/full","width":5000,"height":10},
              {"id":"str","preview_url":"/gifs/media/str/preview","full_url":"/gifs/media/str/full","width":"480","height":270}
            ],"next_offset":24}
        """.trimIndent()
        val page = parseGifPage(body, dev)!!
        assertEquals(listOf("ok1"), page.items.map { it.id })
        assertEquals("$dev/gifs/media/ok1/preview", page.items[0].previewUrl)
        assertEquals(24, page.nextOffset)
        assertNull(parseGifPage("""{"nope":1}""", dev))
        assertNull(parseGifPage("<html>", dev))
        assertNull(parseGifPage("""{"items":[],"next_offset":null}""", dev)!!.nextOffset)
    }

    @Test
    fun failureMapping() {
        assertEquals(GifResult.NotConfigured, failureFromStatus(503, null, """{"error":"gif-provider-not-configured"}"""))
        // A 503 from nginx in front of a dead service is an outage, not "hide the tab".
        assertEquals(GifResult.Unavailable, failureFromStatus(503, null, "<html>Service Unavailable</html>"))
        assertEquals(GifResult.Unavailable, failureFromStatus(503, null, null))
        assertEquals(GifResult.RateLimited(7), failureFromStatus(429, "7", null))
        assertEquals(GifResult.RateLimited(2), failureFromStatus(429, "1.2", null))
        assertEquals(GifResult.RateLimited(1), failureFromStatus(429, "soon", null))
        assertEquals(GifResult.Unauthorized, failureFromStatus(401, null, null))
        assertEquals(GifResult.Unavailable, failureFromStatus(502, null, null))
    }

    @Test
    fun pageSendsTheBearerTokenAndParses() = runTest {
        var seenUrl = ""
        var seenAuth: String? = null
        val engine = MockEngine { request ->
            seenUrl = request.url.toString()
            seenAuth = request.headers[HttpHeaders.Authorization]
            respond(
                """{"items":[{"id":"a1","preview_url":"/gifs/media/a1/preview","full_url":"/gifs/media/a1/full","width":10,"height":10}],"next_offset":null}""",
                HttpStatusCode.OK,
                headersOf(HttpHeaders.ContentType, "application/json"),
            )
        }
        val client = GifClient(baseUrl = prod, sessionToken = { "tok" }, engine = engine)
        val result = client.page("dog", 0)
        assertEquals("https://api.lakko.me/gifs/gifs/search?q=dog&offset=0", seenUrl)
        assertEquals("Bearer tok", seenAuth)
        assertEquals(listOf("a1"), (result as GifResult.Ok).value.items.map { it.id })
    }

    @Test
    fun notConfiguredAndRateLimitedResponses() = runTest {
        val notConfigured = MockEngine {
            respond("""{"error":"gif-provider-not-configured"}""", HttpStatusCode.ServiceUnavailable)
        }
        assertEquals(GifResult.NotConfigured, GifClient(dev, { "t" }, notConfigured).page("", 0))
        val limited = MockEngine {
            respond(
                """{"error":"rate-limited"}""",
                HttpStatusCode.TooManyRequests,
                headersOf(HttpHeaders.RetryAfter, "3"),
            )
        }
        assertEquals(GifResult.RateLimited(3), GifClient(dev, { "t" }, limited).page("x", 0))
        val upstream = MockEngine { respondError(HttpStatusCode.BadGateway) }
        assertEquals(GifResult.Unavailable, GifClient(dev, { "t" }, upstream).page("x", 0))
    }

    @Test
    fun noSessionTokenMakesNoRequest() = runTest {
        var requests = 0
        val engine = MockEngine {
            requests += 1
            respond("{}", HttpStatusCode.OK)
        }
        assertEquals(GifResult.Unauthorized, GifClient(dev, { null }, engine).page("x", 0))
        assertEquals(0, requests)
    }

    @Test
    fun mediaRefusesForeignUrlsWrongTypesAndOversizeBodies() = runTest {
        var requests = 0
        val bytes = ByteArray(64) { it.toByte() }
        val engine = MockEngine { request ->
            requests += 1
            when (request.url.encodedPath) {
                "/gifs/media/ok/full" -> respond(ByteReadChannel(bytes), HttpStatusCode.OK, headersOf(HttpHeaders.ContentType, "image/gif; charset=binary"))
                "/gifs/media/html/full" -> respond(ByteReadChannel(bytes), HttpStatusCode.OK, headersOf(HttpHeaders.ContentType, "text/html"))
                else -> respond(ByteReadChannel(ByteArray(200)), HttpStatusCode.OK, headersOf(HttpHeaders.ContentType, "image/webp"))
            }
        }
        val client = GifClient(dev, { "t" }, engine)
        val ok = client.media("$dev/gifs/media/ok/full", maxBytes = 64) as GifResult.Ok
        assertTrue(bytes.contentEquals(ok.value.bytes))
        assertEquals("image/gif", ok.value.mime)
        assertEquals(GifResult.Unavailable, client.media("$dev/gifs/media/html/full", 64))
        assertEquals(GifResult.Unavailable, client.media("$dev/gifs/media/big/full", 100))
        val before = requests
        assertEquals(GifResult.Unavailable, client.media("https://media.giphy.com/gifs/media/ok/full", 64))
        assertEquals(before, requests)
    }

    @Test
    fun debouncerRunsOnlyTheLastCallAfterTheQuietPeriod() = runTest {
        val ran = ArrayList<String>()
        val debouncer = SearchDebouncer(this, delayMs = GIF_SEARCH_DEBOUNCE_MS)
        debouncer.schedule { ran += "c" }
        advanceTimeBy(200)
        debouncer.schedule { ran += "ca" }
        advanceTimeBy(200)
        debouncer.schedule { ran += "cat" }
        advanceTimeBy(GIF_SEARCH_DEBOUNCE_MS - 1)
        runCurrent()
        assertEquals(emptyList<String>(), ran)
        advanceTimeBy(2)
        runCurrent()
        assertEquals(listOf("cat"), ran)
    }

    @Test
    fun debouncerFlushAndCancel() = runTest {
        val ran = ArrayList<String>()
        val debouncer = SearchDebouncer(this)
        debouncer.schedule { ran += "now" }
        debouncer.flush()
        runCurrent()
        assertEquals(listOf("now"), ran)
        debouncer.schedule { ran += "never" }
        debouncer.cancel()
        advanceTimeBy(10_000)
        runCurrent()
        assertEquals(listOf("now"), ran)
    }

    private fun item(id: String) = GifItem(id, "$dev/gifs/media/$id/preview", "$dev/gifs/media/$id/full", 10, 10)

    @Test
    fun pickerLoadsTrendingDebouncesSearchAndHidesWhenNotConfigured() = runTest {
        val queries = ArrayList<Pair<String, Int>>()
        var answer: GifResult<GifPage> = GifResult.Ok(GifPage(listOf(item("t1")), nextOffset = 24))
        val model = GifPickerModel(this, fetch = { q, o ->
            queries += q to o
            answer
        })
        model.open()
        runCurrent()
        assertEquals(listOf("" to 0), queries)
        assertEquals(GifPickerModel.Phase.READY, model.state.value.phase)

        model.onQueryChange("c")
        model.onQueryChange("ca")
        model.onQueryChange("cat")
        advanceTimeBy(GIF_SEARCH_DEBOUNCE_MS + 1)
        runCurrent()
        assertEquals(listOf("" to 0, "cat" to 0), queries)

        answer = GifResult.Ok(GifPage(emptyList(), null))
        model.onQueryChange("zzz")
        advanceTimeBy(GIF_SEARCH_DEBOUNCE_MS + 1)
        runCurrent()
        assertEquals(GifPickerModel.Phase.EMPTY, model.state.value.phase)

        answer = GifResult.RateLimited(2)
        model.retry()
        runCurrent()
        assertEquals(GifPickerModel.Phase.RATE_LIMITED, model.state.value.phase)

        answer = GifResult.NotConfigured
        model.retry()
        runCurrent()
        assertEquals(GifPickerModel.Phase.HIDDEN, model.state.value.phase)
        // Hidden stays hidden: typing sends nothing more.
        val count = queries.size
        model.onQueryChange("dog")
        advanceTimeBy(GIF_SEARCH_DEBOUNCE_MS + 1)
        runCurrent()
        assertEquals(count, queries.size)
    }

    @Test
    fun pickerPagesAndKeepsItemsWhenLoadingMoreFails() = runTest {
        var answer: GifResult<GifPage> = GifResult.Ok(GifPage(listOf(item("a"), item("b")), nextOffset = 2))
        val model = GifPickerModel(this, fetch = { _, _ -> answer })
        model.open()
        runCurrent()
        answer = GifResult.Ok(GifPage(listOf(item("b"), item("c")), nextOffset = 4))
        model.loadMore()
        runCurrent()
        assertEquals(listOf("a", "b", "c"), model.state.value.items.map { it.id })
        assertEquals(4, model.state.value.nextOffset)
        answer = GifResult.Unavailable
        model.loadMore()
        runCurrent()
        assertEquals(listOf("a", "b", "c"), model.state.value.items.map { it.id })
        assertEquals(GifPickerModel.Phase.READY, model.state.value.phase)
        assertNull(model.state.value.nextOffset)
    }

    @Test
    fun aSlowAnswerForAnOldQueryNeverOverwritesANewerOne() = runTest {
        val gates = HashMap<String, kotlinx.coroutines.CompletableDeferred<GifResult<GifPage>>>()
        val model = GifPickerModel(this, fetch = { q, _ ->
            gates.getOrPut(q) { kotlinx.coroutines.CompletableDeferred() }.await()
        })
        model.onQueryChange("old")
        model.submit()
        runCurrent()
        model.onQueryChange("new")
        model.submit()
        runCurrent()
        gates.getValue("new").complete(GifResult.Ok(GifPage(listOf(item("new1")), null)))
        runCurrent()
        gates.getValue("old").complete(GifResult.Ok(GifPage(listOf(item("old1")), null)))
        runCurrent()
        assertEquals(listOf("new1"), model.state.value.items.map { it.id })
        assertEquals("new", model.state.value.query)
    }
}
