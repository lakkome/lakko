// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.engine.okhttp.OkHttp
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.config.ApiService
import me.lakko.app.transport.AntiCensorshipEngine
import io.ktor.client.plugins.HttpTimeout
import io.ktor.client.plugins.contentnegotiation.ContentNegotiation
import io.ktor.client.plugins.timeout
import io.ktor.client.request.HttpRequestBuilder
import io.ktor.client.request.header
import io.ktor.client.request.request
import io.ktor.client.request.setBody
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.HttpHeaders
import io.ktor.http.HttpMethod
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import io.ktor.serialization.kotlinx.json.json
import kotlin.coroutines.cancellation.CancellationException
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import me.lakko.app.auth.PowChallengeResponse
import me.lakko.app.auth.PowProofPayload
import me.lakko.app.auth.base64Decode
import me.lakko.app.auth.base64Encode

/**
 * Ktor-based client for `lakko-svc-ds`.
 *
 * Mirror of `apps/web/src/ds/client.ts`. The Delivery Service is
 * a thin append-only relay: every message goes into either a
 * per-group journal (`/ds/group/{group_hex}`) or a per-mailbox
 * journal (`/ds/mailbox/{device_hex}`). Both paths key the
 * journal by a 32-byte pseudonym encoded as **lowercase hex** in
 * the URL.
 *
 * Two operations per journal kind:
 *
 *   * `POST /ds/{kind}/{pseudonym_hex}/messages` —
 *     body is raw bytes (`application/octet-stream`), server
 *     caps size at its `MAX_MESSAGE_BYTES`. Response is the
 *     assigned monotonic `id` + ISO-8601 `created_at` — the
 *     `id` becomes the `after=` cursor for the next GET.
 *
 *   * `GET /ds/{kind}/{pseudonym_hex}/messages?after=&limit=&wait=`
 *     — pull all messages newer than `after`. `wait` is the
 *     long-poll budget in seconds (0 returns immediately
 *     whatever's there; non-zero holds the connection until at
 *     least one row arrives or the server's `MAX_WAIT_SECS`
 *     clamp kicks in). Response is a JSON envelope carrying the
 *     rows as `payload_b64` strings; we decode to raw bytes
 *     before handing back.
 *
 * ## Auth posture (ADR-0053)
 *
 * A CLAIMED journal is opened only by a request carrying an
 * `x-lakko-ds-proof` header signed with a key it was claimed under: the
 * per-epoch access key every member derives from the MLS group for a group
 * journal, the identity key for a mailbox read. This client signs every
 * group request and every mailbox read with the signer the call site passes
 * or, failing that, the one [DsSignerRegistry] holds for the journal; a
 * journal no signer is known for goes out unsigned (an unclaimed journal
 * accepts that, a rendezvous mailbox is never claimed). Mailbox WRITES are
 * never signed: the cold inbox stays open. The non-member's one act is the
 * PoW-gated [knock].
 *
 * ## Default base URL
 *
 * `http://10.0.2.2:8082` — Android-emulator loopback alias, port
 * `8082` matching `lakko-svc-ds`'s dev bind. Override via the
 * constructor for physical devices (LAN IP) and production
 * (Cloudflare tunnel URL).
 */
public class DsClient(
    public val baseUrl: String = DEFAULT_BASE_URL,
    /** Ktor engine. Production: `OkHttp` (default below). Tests:
     *  `MockEngine` for deterministic responses without a
     *  network. */
    engine: HttpClientEngine? = null,
    /** ADR-0053 — where a claimed journal's signer is looked up when the
     *  call site passes none. */
    private val signers: DsSignerRegistry = DsSignerRegistry.shared,
    /** Device clock, injected so the skew rule is testable. */
    private val nowMs: () -> Long = System::currentTimeMillis,
) : AutoCloseable {

    private val json = Json {
        ignoreUnknownKeys = true
        explicitNulls = false
    }

    private val client: HttpClient = if (engine != null) {
        HttpClient(engine) {
            install(ContentNegotiation) { json(json) }
            installTimeouts()
        }
    } else {
        HttpClient(AntiCensorshipEngine(OkHttp.create {})) {
            install(ContentNegotiation) { json(json) }
            installTimeouts()
        }
    }

    override fun close() {
        client.close()
    }

    // -----------------------------------------------------------------
    // Mailbox: POST publish
    // -----------------------------------------------------------------

    /**
     * `POST /ds/mailbox/{pseudonym_hex}/messages` — publish a
     * raw payload to a mailbox. Body is sent as
     * `application/octet-stream`; the server caps the size.
     *
     * ADR-0053: never signed. Writing into a mailbox stays open (the cold
     * inbox), and a rendezvous mailbox is unclaimed by design.
     *
     * @param pseudonym 32-byte recipient pseudonym.
     * @param payload   non-empty bytes to deliver.
     */
    public suspend fun publishMailbox(
        pseudonym: ByteArray,
        payload: ByteArray,
    ): PublishResult {
        if (pseudonym.size != PSEUDONYM_LEN) {
            throw DsClientArgError("pseudonym must be $PSEUDONYM_LEN bytes")
        }
        if (payload.isEmpty()) {
            throw DsClientArgError("payload must be non-empty")
        }
        return doPublish(
            endpoint = "/ds/mailbox/${pseudonymHex(pseudonym)}/messages",
            payload = payload,
            journal = null,
        )
    }

    /**
     * `POST /ds/group/{group_id_hex}/messages` — publish a raw
     * payload to a group journal.
     *
     * ADR-0053: signed with [signer] when given, else with the registry's
     * signer for this journal (unsigned when neither knows it — an unclaimed
     * journal accepts that). A refusal retries per the client contract.
     */
    public suspend fun publishGroup(
        groupId: ByteArray,
        payload: ByteArray,
        signer: DsJournalSigner? = null,
    ): PublishResult {
        if (groupId.isEmpty()) {
            throw DsClientArgError("groupId must be non-empty")
        }
        if (payload.isEmpty()) {
            throw DsClientArgError("payload must be non-empty")
        }
        return doPublish(
            endpoint = "/ds/group/${pseudonymHex(groupId)}/messages",
            payload = payload,
            journal = Journal.group(groupId, signer),
        )
    }

    /** Shared POST plumbing for the two publish flavours.
     *  Octet-stream body + JSON response. */
    private suspend fun doPublish(endpoint: String, payload: ByteArray, journal: Journal?): PublishResult {
        val response = exchange(
            method = HttpMethod.Post,
            endpoint = endpoint,
            query = "",
            body = payload,
            contentType = ContentType.Application.OctetStream,
            waitSecs = null,
            journal = journal,
        )
        if (!response.status.isSuccess()) {
            throw DsHttpError(
                status = response.status.value,
                endpoint = endpoint,
                body = response.safeReadBody(),
            )
        }
        val parsed: PublishWireResponse = response.body()
        return PublishResult(id = parsed.id, createdAt = parsed.created_at)
    }

    // -----------------------------------------------------------------
    // Mailbox / Group: GET fetch with cursor
    // -----------------------------------------------------------------

    /**
     * `GET /ds/mailbox/{pseudonym_hex}/messages?after=&limit=&wait=`
     * — pull all messages newer than `after` for the given
     * mailbox.
     *
     * `waitSecs` is the long-poll budget. `0` returns immediately
     * (good for catch-up fetches); a non-zero value holds the
     * connection up to that many seconds — typical 30-60 for an
     * active poll loop. The server clamps to its own `MAX_WAIT_SECS`.
     *
     * ADR-0053: a claimed mailbox is read only under its identity key —
     * [signer], else the registry's signer for this mailbox. A rendezvous
     * mailbox is never in the registry and is read unsigned.
     */
    public suspend fun fetchMailbox(
        pseudonym: ByteArray,
        after: Long? = null,
        limit: Int? = null,
        waitSecs: Int? = null,
        signer: DsJournalSigner? = null,
    ): FetchResult {
        if (pseudonym.size != PSEUDONYM_LEN) {
            throw DsClientArgError("pseudonym must be $PSEUDONYM_LEN bytes")
        }
        return doFetch(
            endpoint = "/ds/mailbox/${pseudonymHex(pseudonym)}/messages",
            after = after,
            limit = limit,
            waitSecs = waitSecs,
            journal = Journal.mailbox(pseudonym, signer),
        )
    }

    /**
     * `GET /ds/group/{group_id_hex}/messages?after=&limit=&wait=`
     * — pull all messages newer than `after` for the given group
     * journal. Mirror of [fetchMailbox].
     *
     * [reclaimOnRefusal] is `false` for a background reader: it writes
     * nothing, a claim included (C-59).
     */
    public suspend fun fetchGroup(
        groupId: ByteArray,
        after: Long? = null,
        limit: Int? = null,
        waitSecs: Int? = null,
        signer: DsJournalSigner? = null,
        reclaimOnRefusal: Boolean = true,
    ): FetchResult {
        if (groupId.isEmpty()) {
            throw DsClientArgError("groupId must be non-empty")
        }
        return doFetch(
            endpoint = "/ds/group/${pseudonymHex(groupId)}/messages",
            after = after,
            limit = limit,
            waitSecs = waitSecs,
            journal = Journal.group(groupId, signer, reclaimOnRefusal),
        )
    }

    /**
     * ADR-0053 §4 — `GET /ds/group/{hex}/knocks?after=&limit=&wait=`: what
     * non-members knocked (an external-join proposal and its attribution).
     * Signed exactly like a group read, its own id space and cursor.
     */
    public suspend fun fetchKnocks(
        groupId: ByteArray,
        after: Long? = null,
        limit: Int? = null,
        waitSecs: Int? = null,
        signer: DsJournalSigner? = null,
    ): FetchResult {
        if (groupId.isEmpty()) {
            throw DsClientArgError("groupId must be non-empty")
        }
        return doFetch(
            endpoint = "/ds/group/${pseudonymHex(groupId)}/knocks",
            after = after,
            limit = limit,
            waitSecs = waitSecs,
            journal = Journal.group(groupId, signer),
        )
    }

    private suspend fun doFetch(
        endpoint: String,
        after: Long?,
        limit: Int?,
        waitSecs: Int?,
        journal: Journal?,
    ): FetchResult {
        val response = exchange(
            method = HttpMethod.Get,
            endpoint = endpoint,
            query = fetchQuery(after = after, limit = limit, waitSecs = waitSecs),
            body = ByteArray(0),
            contentType = null,
            waitSecs = waitSecs,
            journal = journal,
        )
        if (!response.status.isSuccess()) {
            throw DsHttpError(
                status = response.status.value,
                endpoint = endpoint,
                body = response.safeReadBody(),
            )
        }
        val parsed: FetchWireResponse = response.body()
        return FetchResult(
            messages = parsed.messages.map {
                MailboxMessage(
                    id = it.id,
                    payload = base64Decode(it.payload_b64),
                    createdAt = it.created_at,
                )
            },
            hasMore = parsed.has_more,
        )
    }

    // -----------------------------------------------------------------
    // ADR-0053: claims and registrations
    // -----------------------------------------------------------------

    /**
     * `PUT /ds/group/{hex}/access` registering [signer]'s key at its epoch,
     * proven by [provenBy] when given (unsigned otherwise: the first claim of
     * an unclaimed journal, or a re-send of an already-registered key, needs
     * no proof). Throws [DsHttpError] on a refusal.
     */
    public suspend fun claimGroup(
        groupId: ByteArray,
        signer: DsJournalSigner,
        provenBy: DsJournalSigner? = null,
    ) {
        val endpoint = "/ds/group/${pseudonymHex(groupId)}/access"
        val body = signer.groupClaimBody().toByteArray(Charsets.UTF_8)
        val response = send(
            method = HttpMethod.Put,
            endpoint = endpoint,
            query = "",
            body = body,
            contentType = ContentType.Application.Json,
            waitSecs = null,
            signer = provenBy,
        )
        if (!response.status.isSuccess()) {
            throw proofOrHttpError(response, endpoint)
        }
    }

    /**
     * Register the key of the epoch a commit moves to ([next]) proven under
     * the epoch it leaves ([current]) — ADR-0053 client contract point 3,
     * run BEFORE the commit is persisted or published. On a refusal, retry
     * once after re-claiming [current] (proven by the registry's previous
     * signer for this journal, when held): the one case that repairs is a
     * journal whose registration of [current] never landed. Throws on a
     * second refusal; the caller aborts the commit.
     */
    public suspend fun registerNextEpoch(
        groupId: ByteArray,
        next: DsJournalSigner,
        current: DsJournalSigner,
    ) {
        try {
            claimGroup(groupId, next, provenBy = current)
            return
        } catch (e: DsHttpError) {
            if (e.status != HTTP_UNAUTHORIZED && e.status != HTTP_CONFLICT) throw e
        }
        val previous = signers.previous(dsGroupKey(groupId))?.takeIf { it.epoch < current.epoch }
        try {
            claimGroup(groupId, current, provenBy = previous)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            // The retry below says whether it helped.
        }
        claimGroup(groupId, next, provenBy = current)
    }

    /**
     * `PUT /ds/mailbox/{hex}/access` claiming the mailbox [signer]'s identity
     * key names ([pseudonym]). Unsigned by design. Throws on a refusal.
     */
    public suspend fun claimMailbox(pseudonym: ByteArray, signer: DsJournalSigner) {
        if (pseudonym.size != PSEUDONYM_LEN) {
            throw DsClientArgError("pseudonym must be $PSEUDONYM_LEN bytes")
        }
        val endpoint = "/ds/mailbox/${pseudonymHex(pseudonym)}/access"
        val response = send(
            method = HttpMethod.Put,
            endpoint = endpoint,
            query = "",
            body = signer.mailboxClaimBody().toByteArray(Charsets.UTF_8),
            contentType = ContentType.Application.Json,
            waitSecs = null,
            signer = null,
        )
        if (!response.status.isSuccess()) {
            throw proofOrHttpError(response, endpoint)
        }
    }

    // -----------------------------------------------------------------
    // ADR-0053 §4: the knock
    // -----------------------------------------------------------------

    /** `POST /pow/challenge` on the DS — a fresh proof-of-work challenge for
     *  one knock. */
    public suspend fun powChallenge(): PowChallengeResponse {
        val endpoint = "/pow/challenge"
        val response = send(
            method = HttpMethod.Post,
            endpoint = endpoint,
            query = "",
            body = ByteArray(0),
            contentType = null,
            waitSecs = null,
            signer = null,
        )
        if (!response.status.isSuccess()) {
            throw DsHttpError(response.status.value, endpoint, response.safeReadBody())
        }
        return response.body()
    }

    /**
     * `POST /ds/group/{hex}/knock` with one solved [pow]: [payloads] are the
     * rows (at most four, 16 KiB in total); an EMPTY list is a probe that
     * stores nothing and only learns the group's epoch.
     */
    public suspend fun knock(
        groupId: ByteArray,
        payloads: List<ByteArray>,
        pow: PowProofPayload,
    ): KnockResult {
        if (groupId.isEmpty()) {
            throw DsClientArgError("groupId must be non-empty")
        }
        val endpoint = "/ds/group/${pseudonymHex(groupId)}/knock"
        val body = json.encodeToString(
            KnockWireRequest.serializer(),
            KnockWireRequest(pow = pow, payloads_b64 = payloads.map(::base64Encode)),
        ).toByteArray(Charsets.UTF_8)
        val response = send(
            method = HttpMethod.Post,
            endpoint = endpoint,
            query = "",
            body = body,
            contentType = ContentType.Application.Json,
            waitSecs = null,
            signer = null,
        )
        if (!response.status.isSuccess()) {
            throw DsHttpError(response.status.value, endpoint, response.safeReadBody())
        }
        val parsed: KnockWireResponse = response.body()
        return KnockResult(ids = parsed.ids, epoch = parsed.epoch?.toULong())
    }

    /** One knock including its challenge: fetch a challenge, [solve] it,
     *  knock. Each knock pays its own proof of work. */
    public suspend fun knockWithPow(
        groupId: ByteArray,
        payloads: List<ByteArray>,
        solve: suspend (seedB64: String, effortBits: Int) -> PowProofPayload,
    ): KnockResult {
        val challenge = powChallenge()
        val pow = solve(challenge.seed, challenge.effort_bits)
        return knock(groupId, payloads, pow)
    }

    // -----------------------------------------------------------------
    // The request core
    // -----------------------------------------------------------------

    /** Which journal a request names, and how it may be signed. */
    private class Journal(
        val key: String,
        val kind: Kind,
        val pseudonym: ByteArray,
        val explicit: DsJournalSigner?,
        val reclaim: Boolean,
    ) {
        enum class Kind { GROUP, MAILBOX }

        companion object {
            fun group(groupId: ByteArray, explicit: DsJournalSigner?, reclaim: Boolean = true) =
                Journal(dsGroupKey(groupId), Kind.GROUP, groupId, explicit, reclaim)

            fun mailbox(pseudonym: ByteArray, explicit: DsJournalSigner?) =
                Journal(dsMailboxKey(pseudonym), Kind.MAILBOX, pseudonym, explicit, reclaim = true)
        }
    }

    /**
     * ADR-0053 client contract points 1 and 5: sign with the journal's
     * current signer; on a `401`, once more with the previous epoch's signer
     * (a read that raced a lagging registration), then once after a
     * claim-on-open; a third refusal surfaces as [DsProofRefusedError],
     * blaming the clock when the response `Date` says so. A request to a
     * journal no signer is known for goes out unsigned, and its `401` is
     * surfaced at once.
     */
    private suspend fun exchange(
        method: HttpMethod,
        endpoint: String,
        query: String,
        body: ByteArray,
        contentType: ContentType?,
        waitSecs: Int?,
        journal: Journal?,
    ): HttpResponse {
        val first = journal?.let { it.explicit ?: signers.current(it.key) }
        var response = send(method, endpoint, query, body, contentType, waitSecs, first)
        if (journal == null || response.status.value != HTTP_UNAUTHORIZED) return response
        if (first == null) throw refusedError(response, endpoint)

        val previous = if (journal.explicit == null) signers.previous(journal.key) else null
        if (previous != null && previous.epoch != first.epoch) {
            response = send(method, endpoint, query, body, contentType, waitSecs, previous)
            if (response.status.value != HTTP_UNAUTHORIZED) return response
        }
        if (journal.reclaim) {
            val reclaimed = try {
                when (journal.kind) {
                    Journal.Kind.GROUP -> claimGroup(journal.pseudonym, first, provenBy = previous)
                    Journal.Kind.MAILBOX -> claimMailbox(journal.pseudonym, first)
                }
                true
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (_: Throwable) {
                false
            }
            if (reclaimed) {
                val again = journal.explicit ?: signers.current(journal.key) ?: first
                response = send(method, endpoint, query, body, contentType, waitSecs, again)
                if (response.status.value != HTTP_UNAUTHORIZED) return response
            }
        }
        throw refusedError(response, endpoint)
    }

    private suspend fun refusedError(response: HttpResponse, endpoint: String): DsProofRefusedError =
        DsProofRefusedError(
            endpoint = endpoint,
            clockSkewed = dsClockSkewed(response.headers[HttpHeaders.Date], nowMs()),
            body = response.safeReadBody(),
        )

    private suspend fun proofOrHttpError(response: HttpResponse, endpoint: String): DsHttpError =
        if (response.status.value == HTTP_UNAUTHORIZED) {
            refusedError(response, endpoint)
        } else {
            DsHttpError(response.status.value, endpoint, response.safeReadBody())
        }

    /**
     * One HTTP exchange. The query string is built by the caller and used
     * both for the URL and for the signature, so the two cannot differ; the
     * signature covers the SERVICE path ([endpoint]), never the nginx prefix
     * in [baseUrl].
     */
    private suspend fun send(
        method: HttpMethod,
        endpoint: String,
        query: String,
        body: ByteArray,
        contentType: ContentType?,
        waitSecs: Int?,
        signer: DsJournalSigner?,
    ): HttpResponse {
        val url = if (query.isEmpty()) "$baseUrl$endpoint" else "$baseUrl$endpoint?$query"
        val proof = signer?.let {
            try {
                it.sign(method.value, endpoint, query, body)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (cause: Throwable) {
                // A signer whose handle is gone: send unsigned, which a claimed
                // journal refuses and the retry path above then handles.
                null
            }
        }
        return try {
            client.request(url) {
                this.method = method
                if (proof != null) header(DS_PROOF_HEADER, proof)
                if (method != HttpMethod.Get) {
                    if (contentType != null) contentType(contentType)
                    setBody(body)
                }
                configureLongPoll(waitSecs)
            }
        } catch (cancellation: CancellationException) {
            // G-37 — structured concurrency: a genuine cancellation is NOT a
            // network failure and must propagate verbatim, never be re-labelled
            // as [DsNetworkError] (which callers convert into a `ChatError` and
            // surface to the user as a failed send).
            //
            // This is also what keeps the timeout unambiguous. Ktor's
            // [io.ktor.client.plugins.HttpRequestTimeoutException] extends
            // `java.io.IOException`, NOT `CancellationException` — so a timeout
            // falls through to the branch below and becomes a [DsNetworkError],
            // while a real cancellation takes this branch.
            throw cancellation
        } catch (cause: Throwable) {
            throw DsNetworkError(endpoint, cause)
        }
    }

    private fun HttpRequestBuilder.configureLongPoll(waitSecs: Int?) {
        // G-37 — a long-poll fetch DELIBERATELY holds the connection open for
        // `waitSecs`, so the flat [DEFAULT_REQUEST_TIMEOUT_MS] would sever it by
        // design. Widen this ONE request to the long-poll budget plus slack for
        // the round-trip and the server's own `MAX_WAIT_SECS` clamp. Same shape
        // as iOS (`DsClient.swift` — `timeoutInterval = waitSecs + 10`).
        //
        // `socketTimeoutMillis` has to move too: a long poll is by definition an
        // idle socket until the server has something to say, so leaving it at the
        // default would fire first and defeat the widened request budget.
        if (waitSecs != null && waitSecs > 0) {
            val budgetMs = (waitSecs.toLong() + LONG_POLL_MARGIN_SECS) * MILLIS_PER_SEC
            timeout {
                requestTimeoutMillis = budgetMs
                socketTimeoutMillis = budgetMs
            }
        }
    }

    public companion object {
        /** DS service URL — `<LAKKO_API_BASE_URL>/ds` ([ApiEndpoints]),
         *  or the Android emulator loopback alias + the dev
         *  `lakko-svc-ds` bind (`http://10.0.2.2:8082`) without one. */
        public val DEFAULT_BASE_URL: String
            get() = ApiEndpoints.baseUrlFor(ApiService.DS)

        /** 32-byte pseudonym (BLAKE3 output length used by every
         *  variant in [me.lakko.app...] — see 1.15e2). */
        private const val PSEUDONYM_LEN: Int = 32

        /**
         * Lowercase-hex encode a byte array (no `0x` prefix).
         * The DS path expects exactly `2 * pseudonym.size`
         * lowercase hex chars. Public because it's a useful
         * helper for tests + for any caller assembling DS URLs
         * directly without going through this client.
         */
        public fun pseudonymHex(bytes: ByteArray): String {
            val sb = StringBuilder(bytes.size * 2)
            for (b in bytes) {
                val v = b.toInt() and 0xFF
                sb.append(HEX[v ushr 4])
                sb.append(HEX[v and 0x0F])
            }
            return sb.toString()
        }

        private const val HEX: String = "0123456789abcdef"

        /**
         * G-37 — the default per-request budget for every DS call that is NOT a
         * long poll (every publish, and a `wait=0` catch-up fetch).
         *
         * Before this, nothing configured a timeout at all: the client installed
         * only `ContentNegotiation` over an empty `OkHttp.create {}`, so the
         * effective bound was whatever the engine happened to inherit — unknown,
         * and unbounded as far as this code could tell. Every `publishGroup` /
         * `publishMailbox` call site in `ChatRepository` runs inside the single
         * repository-wide `groupsLock`, so an unbounded publish is an unbounded
         * hold of a lock that every other group's send and commit queues behind.
         */
        private const val DEFAULT_REQUEST_TIMEOUT_MS: Long = 15_000

        /** Connection-establishment budget (separate from the request budget —
         *  a dead host should fail fast rather than burn the whole request
         *  budget on a TCP handshake that will never complete). */
        private const val CONNECT_TIMEOUT_MS: Long = 10_000

        /** Slack added to a long-poll `waitSecs` budget so the round-trip and the
         *  server's own `MAX_WAIT_SECS` clamp can complete without the client
         *  timing out first. Mirrors iOS (`waitSecs + 10`). */
        private const val LONG_POLL_MARGIN_SECS: Long = 10

        private const val MILLIS_PER_SEC: Long = 1_000

        private const val HTTP_UNAUTHORIZED: Int = 401

        private const val HTTP_CONFLICT: Int = 409

        /**
         * ADR-0053 — the query string of a fetch, built ONCE and used both in
         * the URL and in the proof: `after`, `limit`, `wait`, in that order,
         * each only when given. Plain decimal numbers, so nothing in it is
         * ever re-encoded on the way to the server.
         */
        public fun fetchQuery(after: Long?, limit: Int?, waitSecs: Int?): String {
            val parts = ArrayList<String>(3)
            if (after != null) parts += "after=$after"
            if (limit != null) parts += "limit=$limit"
            if (waitSecs != null) parts += "wait=$waitSecs"
            return parts.joinToString("&")
        }

        /**
         * Install the explicit request/connect/socket bounds on a client config.
         *
         * Applied to BOTH the production engine and a caller-supplied test engine
         * so a `MockEngine`-backed test exercises the same timeout behaviour the
         * real client has — otherwise the bound would be untestable on the host.
         */
        private fun io.ktor.client.HttpClientConfig<*>.installTimeouts() {
            install(HttpTimeout) {
                requestTimeoutMillis = DEFAULT_REQUEST_TIMEOUT_MS
                connectTimeoutMillis = CONNECT_TIMEOUT_MS
                socketTimeoutMillis = DEFAULT_REQUEST_TIMEOUT_MS
            }
        }
    }
}

// =============================================================================
// Wire shapes (private — public surface uses the cleaner result types below)
// =============================================================================

@Serializable
@Suppress("ConstructorParameterNaming")
private data class PublishWireResponse(
    val id: Long,
    val created_at: String,
)

@Serializable
@Suppress("ConstructorParameterNaming")
private data class FetchWireResponse(
    val messages: List<FetchWireMessage>,
    val has_more: Boolean,
)

@Serializable
@Suppress("ConstructorParameterNaming")
private data class FetchWireMessage(
    val id: Long,
    val payload_b64: String,
    val created_at: String,
)

@Serializable
@Suppress("ConstructorParameterNaming")
private data class KnockWireRequest(
    val pow: PowProofPayload,
    val payloads_b64: List<String>,
)

@Serializable
private data class KnockWireResponse(
    val ids: List<Long> = emptyList(),
    val epoch: Long? = null,
)

private suspend fun HttpResponse.safeReadBody(): String {
    return try {
        val text = bodyAsText()
        if (text.length > 512) "${text.take(512)}…" else text
    } catch (cause: Throwable) {
        "<unreadable body: ${cause::class.simpleName}>"
    }
}

// =============================================================================
// Public result types
// =============================================================================

/** Result of [DsClient.publishMailbox] / [DsClient.publishGroup]. */
public data class PublishResult(
    /** Server-assigned monotonic id. Use as the `after` cursor on
     *  subsequent fetches. */
    val id: Long,
    /** ISO-8601 timestamp from the server. */
    val createdAt: String,
)

/** Decoded message from a fetch cursor. */
public data class MailboxMessage(
    val id: Long,
    /** Raw payload bytes (decoded from the server's `payload_b64`). */
    val payload: ByteArray,
    val createdAt: String,
) {
    // Override equals/hashCode for the `payload` ByteArray —
    // same reason as the auth Result types: data-class default
    // is reference equality on arrays, which is rarely what
    // anyone expects.
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is MailboxMessage) return false
        return id == other.id &&
            payload.contentEquals(other.payload) &&
            createdAt == other.createdAt
    }

    override fun hashCode(): Int {
        var result = id.hashCode()
        result = 31 * result + payload.contentHashCode()
        result = 31 * result + createdAt.hashCode()
        return result
    }
}

/** Result of [DsClient.fetchMailbox] / [DsClient.fetchGroup]. */
public data class FetchResult(
    val messages: List<MailboxMessage>,
    /** `true` if there are more rows past the last returned id;
     *  caller should call again with `after = lastMessage.id`. */
    val hasMore: Boolean,
)

// =============================================================================
// Errors
// =============================================================================

/** Programmer error — surfaced before any network call. */
public class DsClientArgError(message: String) : IllegalArgumentException(message)

/** Result of [DsClient.knock]: the stored row ids (empty for a probe) and
 *  the group's latest registered access epoch — the epoch a joiner's
 *  external-join proposal must target — or `null` for an unclaimed group. */
public data class KnockResult(
    val ids: List<Long>,
    val epoch: ULong?,
)

/** Server returned a non-2xx. */
public open class DsHttpError(
    public val status: Int,
    public val endpoint: String,
    public val body: String,
    message: String = "ds $endpoint → $status: $body",
) : RuntimeException(message)

/**
 * ADR-0053 — a claimed journal refused this device's proof after every retry
 * the client contract allows (previous-epoch signer, claim-on-open).
 * [clockSkewed] is `true` when the response `Date` is more than a minute from
 * the device clock; [userMessage] is then [DS_CLOCK_SKEW_COPY].
 */
public class DsProofRefusedError(
    endpoint: String,
    public val clockSkewed: Boolean,
    body: String,
) : DsHttpError(
    status = 401,
    endpoint = endpoint,
    body = body,
    message = if (clockSkewed) DS_CLOCK_SKEW_COPY else "ds $endpoint → 401: proof refused",
) {
    /** What the user reads, or `null` for the ordinary delivery error. */
    public val userMessage: String? get() = if (clockSkewed) DS_CLOCK_SKEW_COPY else null
}

/** Network-level failure (DNS, TLS, no route, …). */
public class DsNetworkError(
    public val endpoint: String,
    cause: Throwable,
) : RuntimeException("ds $endpoint network error: $cause", cause)
