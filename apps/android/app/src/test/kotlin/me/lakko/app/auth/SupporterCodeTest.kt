// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * Supporter codes on Android (ADR-0097, `docs/ux/supporters.md` section 1):
 * the exchange, the held place, the bind header and the used outcome. The
 * native core is faked: the host JVM suite has no native library.
 */
class SupporterCodeTest {

    // ---- the field ----

    @Test
    fun `continue needs sixteen letters and digits`() {
        assertTrue(supporterCodeComplete("K7QM-2XRA-9DPF-4WNB"))
        assertTrue(supporterCodeComplete("k7qm 2xra 9dpf 4wnb"))
        assertFalse(supporterCodeComplete("K7QM-2XRA-9DPF-4WN"))
        assertFalse(supporterCodeComplete("K7QM-2XRA-9DPF-4WNBX"))
        assertFalse(supporterCodeComplete(""))
    }

    // ---- the exchange ----

    private class FakeRequest(override val epoch: Long, private val finalizeThrows: Boolean = false) :
        SupporterCodeRequestHandle {
        var closed = false
        var finalizedWith: Triple<String, String, String>? = null

        override fun redeemBodyJson(): String = """{"epoch":$epoch}"""

        override fun finalize(evaluationB64url: String, proofB64url: String, publicKeyB64url: String): String {
            if (finalizeThrows) throw IllegalStateException("proof-verification")
            finalizedWith = Triple(evaluationB64url, proofB64url, publicKeyB64url)
            return "token-$epoch"
        }

        override fun close() {
            closed = true
        }
    }

    private class FakeCore(
        private val epochNow: Long = 10,
        private val finalizeThrows: Boolean = false,
    ) : SupporterCodeCore {
        val begun = mutableListOf<Pair<String, Long>>()
        val requests = mutableListOf<FakeRequest>()

        override fun normalize(text: String): String? =
            if (supporterCodeComplete(text)) text.uppercase().filter { it.isLetterOrDigit() } else null

        override fun epochAt(unixSecs: Long): Long = epochNow

        override val windowSecs: Long = 30L * 86_400L

        override fun begin(code: String, epoch: Long): SupporterCodeRequestHandle {
            begun += code to epoch
            return FakeRequest(epoch, finalizeThrows).also { requests += it }
        }
    }

    private class FakeApi(
        private val answers: MutableList<SupporterRedeemWire>,
        private val key: String? = "key",
    ) : SupporterCodeApi {
        val bodies = mutableListOf<String>()
        val keyEpochs = mutableListOf<Long>()

        override suspend fun redeem(bodyJson: String): SupporterRedeemWire {
            bodies += bodyJson
            return answers.removeAt(0)
        }

        override suspend fun publicKey(epoch: Long): String? {
            keyEpochs += epoch
            return key
        }
    }

    private val code = "K7QM-2XRA-9DPF-4WNB"

    @Test
    fun `a working code holds a place checked under the key route`() = runTest {
        val core = FakeCore()
        val api = FakeApi(mutableListOf(SupporterRedeemWire.Evaluated(10, "ev", "pr")))
        val outcome = SupporterCodeRedeemer(core, api, nowMs = { 5_000L }).redeem(code)
        assertEquals(SupporterRedeemOutcome.Held(HeldPlace("token-10", 10, 5_000L)), outcome)
        assertEquals(listOf(10L), api.keyEpochs)
        assertEquals(Triple("ev", "pr", "key"), core.requests.single().finalizedWith)
        assertTrue(core.requests.single().closed)
    }

    @Test
    fun `text that is not a code never reaches the server`() = runTest {
        val api = FakeApi(mutableListOf())
        assertEquals(SupporterRedeemOutcome.Unknown, SupporterCodeRedeemer(FakeCore(), api).redeem("nope"))
        assertTrue(api.bodies.isEmpty())
    }

    @Test
    fun `server refusals map to the three lines`() = runTest {
        for ((wire, expected) in listOf(
            SupporterRedeemWire.Unknown to SupporterRedeemOutcome.Unknown,
            SupporterRedeemWire.Limited to SupporterRedeemOutcome.Limited,
            SupporterRedeemWire.Failed to SupporterRedeemOutcome.Failed,
        )) {
            val out = SupporterCodeRedeemer(FakeCore(), FakeApi(mutableListOf(wire))).redeem(code)
            assertEquals(expected, out)
        }
    }

    @Test
    fun `an epoch the server does not issue is retried once at its epoch`() = runTest {
        val core = FakeCore(epochNow = 9)
        val api = FakeApi(
            mutableListOf(
                SupporterRedeemWire.EpochNotIssuable(10),
                SupporterRedeemWire.Evaluated(10, "ev", "pr"),
            ),
        )
        val out = SupporterCodeRedeemer(core, api, nowMs = { 1L }).redeem(code)
        assertEquals(SupporterRedeemOutcome.Held(HeldPlace("token-10", 10, 1L)), out)
        assertEquals(listOf(9L, 10L), core.begun.map { it.second })
    }

    @Test
    fun `a second epoch refusal fails`() = runTest {
        val api = FakeApi(
            mutableListOf(
                SupporterRedeemWire.EpochNotIssuable(10),
                SupporterRedeemWire.EpochNotIssuable(11),
            ),
        )
        assertEquals(SupporterRedeemOutcome.Failed, SupporterCodeRedeemer(FakeCore(epochNow = 9), api).redeem(code))
        assertEquals(2, api.bodies.size)
    }

    @Test
    fun `no key, a bad proof or another epoch fail`() = runTest {
        val noKey = FakeApi(mutableListOf(SupporterRedeemWire.Evaluated(10, "ev", "pr")), key = null)
        assertEquals(SupporterRedeemOutcome.Failed, SupporterCodeRedeemer(FakeCore(), noKey).redeem(code))
        val badProof = FakeApi(mutableListOf(SupporterRedeemWire.Evaluated(10, "ev", "pr")))
        assertEquals(
            SupporterRedeemOutcome.Failed,
            SupporterCodeRedeemer(FakeCore(finalizeThrows = true), badProof).redeem(code),
        )
        val otherEpoch = FakeApi(mutableListOf(SupporterRedeemWire.Evaluated(11, "ev", "pr")))
        assertEquals(SupporterRedeemOutcome.Failed, SupporterCodeRedeemer(FakeCore(), otherEpoch).redeem(code))
    }

    // ---- the redeem answer ----

    @Test
    fun `redeem answers are read as ADR-0097 states`() {
        assertEquals(
            SupporterRedeemWire.Evaluated(7, "e", "p"),
            supporterRedeemWireOf(200, """{"context":"beta-place","epoch":7,"evaluation":"e","proof":"p"}"""),
        )
        assertEquals(
            SupporterRedeemWire.Failed,
            supporterRedeemWireOf(200, """{"context":"ds-knock","epoch":7,"evaluation":"e","proof":"p"}"""),
        )
        for ((status, error) in listOf(
            404 to "supporter-code-unknown",
            410 to "supporter-code-expired",
            409 to "supporter-code-mismatch",
            400 to "supporter-code-malformed",
        )) {
            assertEquals(SupporterRedeemWire.Unknown, supporterRedeemWireOf(status, """{"error":"$error"}"""))
        }
        assertEquals(SupporterRedeemWire.Limited, supporterRedeemWireOf(429, """{"error":"supporter-code-limited"}"""))
        assertEquals(
            SupporterRedeemWire.EpochNotIssuable(12),
            supporterRedeemWireOf(400, """{"error":"epoch-not-issuable","current_epoch":12}"""),
        )
        assertEquals(SupporterRedeemWire.Failed, supporterRedeemWireOf(400, """{"error":"epoch-not-issuable"}"""))
        assertEquals(SupporterRedeemWire.Failed, supporterRedeemWireOf(503, """{"error":"supporter-codes-disabled"}"""))
        assertEquals(SupporterRedeemWire.Failed, supporterRedeemWireOf(500, null))
    }

    @Test
    fun `the redeem and key routes are anonymous`() = runTest {
        val seen = mutableListOf<String>()
        val engine = MockEngine { request ->
            seen += "${request.method.value} ${request.url.encodedPath} auth=${request.headers[HttpHeaders.Authorization]}"
            val body = if (request.url.encodedPath.endsWith("/redeem")) {
                """{"context":"beta-place","epoch":3,"evaluation":"e","proof":"p"}"""
            } else {
                """{"context":"beta-place","epoch":3,"public_key":"pk"}"""
            }
            respond(body, HttpStatusCode.OK, headersOf(HttpHeaders.ContentType, "application/json"))
        }
        val api = KtorSupporterCodeApi(baseUrl = "http://test.invalid", engine = engine)
        assertEquals(SupporterRedeemWire.Evaluated(3, "e", "p"), api.redeem("{}"))
        assertEquals("pk", api.publicKey(3))
        assertEquals(
            listOf(
                "POST /auth/supporter-code/redeem auth=null",
                "GET /auth/tokens/key/beta-place/3 auth=null",
            ),
            seen,
        )
    }

    // ---- the held place ----

    private class MapStorage : MarkerStorage {
        val map = HashMap<String, String>()
        override fun get(key: String): String? = map[key]
        override fun set(key: String, value: String) {
            map[key] = value
        }
        override fun remove(key: String) {
            map.remove(key)
        }
    }

    private val windowSecs = 30L * 86_400L
    private val dayMs = 86_400_000L

    private fun store(storage: MarkerStorage, now: () -> Long) = SupporterPlaceStore(
        storage = storage,
        windowSecs = { windowSecs },
        epochAt = { it / windowSecs },
        nowMs = now,
    )

    @Test
    fun `a held place lasts thirty days`() {
        val storage = MapStorage()
        var now = 100L * windowSecs * 1000L
        val s = store(storage) { now }
        val place = HeldPlace("tok", 100, now)
        s.hold(place)
        assertEquals(place, s.held())
        now += 29 * dayMs
        assertEquals(place, s.held())
        now += 2 * dayMs
        assertNull(s.held())
        assertTrue(storage.map.isEmpty())
    }

    @Test
    fun `a place from an epoch older than the previous one is dropped`() {
        val storage = MapStorage()
        val now = 100L * windowSecs * 1000L
        val s = store(storage) { now }
        // Held "now" by the clock, but for epoch 98: the server accepts only
        // 99 and 100.
        s.hold(HeldPlace("tok", 98, now))
        assertNull(s.held())
    }

    @Test
    fun `an unreadable row reads as no place and is cleared`() {
        val storage = MapStorage()
        storage.map[SupporterPlaceStore.KEY] = "garbage"
        assertNull(store(storage) { 0L }.held())
        assertTrue(storage.map.isEmpty())
    }

    @Test
    fun `a redeemed place is stored and drop forgets it`() = runTest {
        val storage = MapStorage()
        val s = store(storage) { 100L * windowSecs * 1000L }
        val places = StoredSupporterPlaces(
            s,
            SupporterCodeRedeemer(
                FakeCore(epochNow = 100),
                FakeApi(mutableListOf(SupporterRedeemWire.Evaluated(100, "ev", "pr"))),
                nowMs = { 100L * windowSecs * 1000L },
            ),
        )
        val out = places.redeem(code)
        assertTrue(out is SupporterRedeemOutcome.Held)
        assertEquals("token-100", places.held()?.headerValue)
        places.drop()
        assertNull(places.held())
    }

    @Test
    fun `a held place never prints its token`() {
        assertFalse(HeldPlace("secret-token", 1, 2).toString().contains("secret-token"))
    }

    // ---- the bind ----

    private val handle = ByteArray(32) { 7 }

    @Test
    fun `the first bind carries the place in the anonymous-token header`() = runTest {
        val headers = mutableListOf<String?>()
        val client = AuthClient(
            baseUrl = "http://test.invalid",
            engine = MockEngine { request ->
                headers += request.headers[SUPPORTER_PLACE_HEADER]
                respond("""{"bound":true}""", HttpStatusCode.OK, headersOf(HttpHeaders.ContentType, "application/json"))
            },
        )
        client.use {
            it.bindLoginHandle("tok", handle, placeToken = "place-token")
            it.bindLoginHandle("tok", handle)
        }
        assertEquals(listOf("place-token", null), headers)
    }

    @Test
    fun `a used place is its own bind outcome`() = runTest {
        val client = AuthClient(
            baseUrl = "http://test.invalid",
            engine = MockEngine {
                respond(
                    """{"error":"supporter-code-used"}""",
                    HttpStatusCode.Forbidden,
                    headersOf(HttpHeaders.ContentType, "application/json"),
                )
            },
        )
        assertEquals(
            LoginHandleBinding.SupporterCodeUsed,
            client.use { it.bindLoginHandle("tok", handle, placeToken = "p") },
        )
    }

    private class PlaceBinder(private val answer: LoginHandleBinding) : LoginHandleBinder {
        val calls = mutableListOf<String>()

        override suspend fun bind(sessionToken: String, handle: ByteArray): LoginHandleBinding {
            calls += "bind"
            return answer
        }

        override suspend fun bindHoldingPlace(
            sessionToken: String,
            handle: ByteArray,
            placeToken: String,
        ): LoginHandleBinding {
            calls += "bind+$placeToken"
            return answer
        }

        override suspend fun unbind(sessionToken: String): Boolean = true
    }

    private val directory = object : AccountDirectory {
        override suspend fun registerUsername(sessionToken: String, accountId: ByteArray, username: String): String =
            username
    }

    private val handles = LoginHandleSource { name -> NamedHandle(username = name, handle = handle) }

    @Test
    fun `the sign-up claim hands the place to the bind`() = runTest {
        val binder = PlaceBinder(LoginHandleBinding.Bound)
        claimUsernameAtSignup(directory, binder, handles, "tok", ByteArray(16), "alice", placeToken = "pt")
        claimUsernameAtSignup(directory, binder, handles, "tok", ByteArray(16), "alice")
        assertEquals(listOf("bind+pt", "bind"), binder.calls)
    }

    @Test
    fun `a used place fails the claim as a refusal at the cap`() = runTest {
        val binder = PlaceBinder(LoginHandleBinding.SupporterCodeUsed)
        try {
            claimUsernameAtSignup(directory, binder, handles, "tok", ByteArray(16), "alice", placeToken = "pt")
            fail("a used place must fail the claim")
        } catch (err: SupporterCodeUsed) {
            // A RegistrationClosed: the repository deletes and wipes the
            // account the same way.
            assertTrue(isFullState(err))
        }
    }
}
