// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

import me.lakko.app.transport.lakkoHttpEngine
import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.HttpClientEngine
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.config.ApiService
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
import me.lakko.app.anon.AnonTokenContext
import me.lakko.app.anon.AnonTokenSource
import me.lakko.app.anon.AnonTokens
import me.lakko.app.anon.AnonymousHttp
import me.lakko.app.anon.sendWithToken
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
 * accepts that, a rendezvous mailbox is never claimed). A mailbox write is
 * signed by a reach grant, or spends one `mailbox-write` anonymous token
 * (ADR-0078). The non-member's one act is the
 * [knock], which spends one `ds-knock` anonymous token (ADR-0054) and goes
 * out on a separate client with its own connection pool.
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
    /** Engine for the knock (a token route). Production:
     *  [AnonymousHttp.engine]; tests: `null` reuses [engine]. */
    anonEngine: HttpClientEngine? = null,
    /** Where the knock draws its `ds-knock` token. */
    private val anonTokens: AnonTokenSource = AnonTokens.shared,
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
        HttpClient(lakkoHttpEngine()) {
            install(ContentNegotiation) { json(json) }
            installTimeouts()
        }
    }

    /** The knock's client: no session, no proof, its own pool. */
    private val anonClient: HttpClient =
        HttpClient(anonEngine ?: engine ?: AnonymousHttp.engine) {
            install(ContentNegotiation) { json(json) }
            installTimeouts()
        }

    override fun close() {
        client.close()
        anonClient.close()
    }

    // -----------------------------------------------------------------
    // Mailbox: POST publish
    // -----------------------------------------------------------------

    /**
     * `POST /ds/mailbox/{pseudonym_hex}/messages` — write into a person's
     * identity mailbox.
     *
     * ADR-0060 §2 — a claimed mailbox is CLOSED unless its owner chose
     * Everyone: it stores a write only when [reach] signs it with a reach key
     * the owner registered (or with a delegate key next to a delegation). A
     * write with no [reach] to a closed mailbox is answered as stored and
     * dropped, so an unsigned write is only right for a person who opened
     * their mailbox. A reach proof the server does not accept is a
     * [DsReachRefusedError].
     *
     * Rendezvous traffic (pairing, recovery, the welcome courier, the state
     * bundle) does NOT come here: it goes to [publishRendezvous].
     *
     * ADR-0078: [payload] is a sealed mailbox row (the DS refuses anything
     * else with `400`), and a write with no [reach] spends one
     * `mailbox-write` anonymous token on the anonymous client.
     *
     * @param pseudonym 32-byte mailbox address.
     * @param payload   one sealed mailbox row.
     * @param reach     the grant (or delegation) authorising the write.
     */
    public suspend fun publishMailbox(
        pseudonym: ByteArray,
        payload: ByteArray,
        reach: DsReachAuth? = null,
    ): PublishResult {
        if (pseudonym.size != PSEUDONYM_LEN) {
            throw DsClientArgError("pseudonym must be $PSEUDONYM_LEN bytes")
        }
        if (payload.isEmpty()) {
            throw DsClientArgError("payload must be non-empty")
        }
        val endpoint = "/ds/mailbox/${pseudonymHex(pseudonym)}/messages"
        if (reach == null) {
            // ADR-0078: a write with no reach proof (to an Everyone mailbox,
            // an unclaimed one, or a closed one, where it is dropped) spends
            // one `mailbox-write` token, on the anonymous client.
            val response = anonTokens.sendWithToken(AnonTokenContext.MAILBOX_WRITE) { name, value ->
                try {
                    anonClient.request("$baseUrl$endpoint") {
                        method = HttpMethod.Post
                        header(name, value)
                        contentType(ContentType.Application.OctetStream)
                        setBody(payload)
                    }
                } catch (cancellation: CancellationException) {
                    throw cancellation
                } catch (cause: Throwable) {
                    throw DsNetworkError(endpoint, cause)
                }
            }
            if (!response.status.isSuccess()) {
                throw DsHttpError(response.status.value, endpoint, response.safeReadBody())
            }
            val parsed: PublishWireResponse = response.body()
            return PublishResult(id = parsed.id, createdAt = parsed.created_at)
        }
        val response = send(
            method = HttpMethod.Post,
            endpoint = endpoint,
            query = "",
            body = payload,
            contentType = ContentType.Application.OctetStream,
            waitSecs = null,
            signer = reach.signer,
            extraHeaders = reach.delegationHeader
                ?.let { mapOf(DS_DELEGATION_HEADER to it) }
                .orEmpty(),
        )
        if (response.status.value == HTTP_UNAUTHORIZED) {
            throw DsReachRefusedError(endpoint, response.safeReadBody())
        }
        if (!response.status.isSuccess()) {
            throw DsHttpError(response.status.value, endpoint, response.safeReadBody())
        }
        val parsed: PublishWireResponse = response.body()
        return PublishResult(id = parsed.id, createdAt = parsed.created_at)
    }

    /**
     * ADR-0060 §2 — `POST /ds/rendezvous/{hex}/messages`: a write into a
     * random rendezvous id (pairing, recovery, the welcome courier, the state
     * bundle). Open and never claimable; it carries no proof, no delegation
     * and no wake header (the server refuses any of them).
     */
    public suspend fun publishRendezvous(
        rendezvous: ByteArray,
        payload: ByteArray,
    ): PublishResult {
        if (rendezvous.size != PSEUDONYM_LEN) {
            throw DsClientArgError("rendezvous id must be $PSEUDONYM_LEN bytes")
        }
        if (payload.isEmpty()) {
            throw DsClientArgError("payload must be non-empty")
        }
        return doPublish(
            endpoint = "/ds/rendezvous/${pseudonymHex(rendezvous)}/messages",
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
     *
     * ADR-0054: [wake] adds the [DS_WAKE_HEADER] asking the DS to wake the
     * group's subscribed devices once the row is appended. `null` (the
     * default) sends no header and wakes nobody — every control envelope.
     * The header rides every retry of the same request, and the DS wakes only
     * for the attempt that was appended.
     */
    public suspend fun publishGroup(
        groupId: ByteArray,
        payload: ByteArray,
        signer: DsJournalSigner? = null,
        wake: DsWake? = null,
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
            wake = wake,
        )
    }

    /** Shared POST plumbing for the two publish flavours.
     *  Octet-stream body + JSON response. */
    private suspend fun doPublish(
        endpoint: String,
        payload: ByteArray,
        journal: Journal?,
        wake: DsWake? = null,
    ): PublishResult {
        val response = exchange(
            method = HttpMethod.Post,
            endpoint = endpoint,
            query = "",
            body = payload,
            contentType = ContentType.Application.OctetStream,
            waitSecs = null,
            journal = journal,
            wake = wake,
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
     * ADR-0053 / ADR-0060 §2: a personal mailbox is read only under its
     * identity key — [signer], else the registry's signer for this mailbox. An
     * unsigned read is `401` whether the mailbox is claimed or not; a refusal
     * claims the mailbox under the signer and retries once. Rendezvous ids are
     * read with [fetchRendezvous].
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
     * ADR-0060 §2 — `GET /ds/rendezvous/{hex}/messages?after=&limit=&wait=`:
     * read a rendezvous id, unsigned.
     */
    public suspend fun fetchRendezvous(
        rendezvous: ByteArray,
        after: Long? = null,
        limit: Int? = null,
        waitSecs: Int? = null,
    ): FetchResult {
        if (rendezvous.size != PSEUDONYM_LEN) {
            throw DsClientArgError("rendezvous id must be $PSEUDONYM_LEN bytes")
        }
        return doFetch(
            endpoint = "/ds/rendezvous/${pseudonymHex(rendezvous)}/messages",
            after = after,
            limit = limit,
            waitSecs = waitSecs,
            journal = null,
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

    /**
     * ADR-0087 section 5 — `DELETE /ds/group/{hex}/knocks?through=<id>` behind
     * `Reject all`: the rows up to the last one this admin saw go; a request
     * that arrived after stays. Write tier, proven like a read.
     */
    public suspend fun deleteKnocks(groupId: ByteArray, through: Long, signer: DsJournalSigner? = null) {
        if (groupId.isEmpty()) {
            throw DsClientArgError("groupId must be non-empty")
        }
        val endpoint = "/ds/group/${pseudonymHex(groupId)}/knocks"
        val response = exchange(
            method = HttpMethod.Delete,
            endpoint = endpoint,
            query = "through=$through",
            body = ByteArray(0),
            contentType = null,
            waitSecs = null,
            journal = Journal.group(groupId, signer),
        )
        if (!response.status.isSuccess()) {
            throw DsHttpError(status = response.status.value, endpoint = endpoint, body = response.safeReadBody())
        }
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
                    reachPk = it.reach_pk_b64?.let { pk ->
                        runCatching { base64Decode(pk) }.getOrNull()?.takeIf { b -> b.size == 32 }
                    },
                )
            },
            hasMore = parsed.has_more,
            latestEpoch = parsed.latest_epoch?.takeIf { it >= 0 }?.toULong(),
            registeredCommits = parsed.commits?.let { list ->
                list.filter { it.epoch >= 0 && it.id >= 0 }.associate { it.epoch.toULong() to it.id }
            },
        )
    }

    // -----------------------------------------------------------------
    // ADR-0057: journal statements (claim, reset, register, registrars,
    // push binding)
    // -----------------------------------------------------------------

    /**
     * `PUT <statement.path>` with the statement's JSON body and NO proof
     * header — the statement carries its own root or registrar signature.
     * [make] signs a fresh statement per attempt (a statement's nonce is spent
     * on the first try, so a retry must never re-send the same bytes).
     *
     * `409` is retried once: it is either a conflict the retry answers the
     * same way, or a lock race the retry wins. Any other non-2xx throws a
     * [DsHttpError] carrying the status (`400` malformed, `401` a stranger
     * signer or an unclaimed journal on a non-claim route, `403` a rank
     * shortfall, `503` push binding disabled).
     */
    public suspend fun putStatement(make: () -> DsStatement) {
        var attempt = 0
        while (true) {
            attempt += 1
            val statement = make()
            val body = statement.bodyJson.toByteArray(Charsets.UTF_8)
            val response = send(
                method = HttpMethod.Put,
                endpoint = statement.path,
                query = "",
                body = body,
                contentType = ContentType.Application.Json,
                waitSecs = null,
                signer = null,
            )
            if (response.status.isSuccess()) return
            val status = response.status.value
            if (status == HTTP_CONFLICT && attempt < STATEMENT_ATTEMPTS) continue
            throw DsHttpError(status, statement.path, response.safeReadBody())
        }
    }

    /**
     * ADR-0081 decision 1 — `POST /ds/group/{hex}/commit`: the root's or a
     * registrar's `RegisterEpoch` statement for the next epoch together with
     * the commit row, in one request proven by [proof] (the key of the epoch
     * the commit leaves) over the exact body. [make] signs a fresh statement
     * per attempt (its nonce is spent on the first try). `201` is a new row,
     * `200` the identical re-send of one that landed (its original id).
     *
     * `409` is retried once (a lost lock race rolls back undecided); a second
     * `409` throws [DsCommitConflictError]: another commit owns the epoch. Any
     * other refusal throws [DsHttpError] (`401` as [DsProofRefusedError]); a
     * request that did not reach the DS throws [DsNetworkError] — the caller
     * cannot tell whether it landed, and the identical re-send says.
     */
    public suspend fun postCommit(make: () -> DsStatement, proof: DsJournalSigner): DsCommitAnswer {
        var attempt = 0
        while (true) {
            attempt += 1
            val statement = make()
            if (!statement.path.startsWith("/ds/group/") || !statement.path.endsWith("/commit")) {
                throw DsClientArgError("a commit path must name a group journal's commit route")
            }
            val body = statement.bodyJson.toByteArray(Charsets.UTF_8)
            val response = send(
                method = HttpMethod.Post,
                endpoint = statement.path,
                query = "",
                body = body,
                contentType = ContentType.Application.Json,
                waitSecs = null,
                signer = proof,
            )
            val status = response.status.value
            if (status == HTTP_OK || status == HTTP_CREATED) {
                val parsed: PublishWireResponse = response.body()
                return DsCommitAnswer(id = parsed.id, createdAt = parsed.created_at, resent = status == HTTP_OK)
            }
            if (status == HTTP_CONFLICT) {
                if (attempt < STATEMENT_ATTEMPTS) continue
                throw DsCommitConflictError(statement.path, response.safeReadBody())
            }
            throw proofOrHttpError(response, statement.path)
        }
    }

    /**
     * `PUT /ds/mailbox/{hex}/access` claiming the mailbox [signer]'s mailbox
     * key names ([pseudonym], ADR-0078; the body is `{"mailbox_pk_b64"}`).
     * Unsigned by design. Throws on a refusal.
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
    // ADR-0060 §2: reach keys
    // -----------------------------------------------------------------

    /**
     * `PUT /ds/mailbox/{hex}/reach` — register [add] and unregister [remove]
     * reach public keys on the owner's own mailbox, and set the Everyone flag
     * when [open] is given, in one transaction. Signed by the identity key the
     * mailbox is named after ([signer], the mailbox signer). Idempotent: a
     * re-added key has its expiry replaced, a removed absent key is ignored.
     * On a `401` the mailbox is claimed (it may never have been) and the
     * update is sent once more.
     */
    public suspend fun putMailboxReach(
        pseudonym: ByteArray,
        signer: DsJournalSigner,
        add: List<DsReachKeyAdd>,
        remove: List<ByteArray>,
        open: Boolean? = null,
    ) {
        if (pseudonym.size != PSEUDONYM_LEN) {
            throw DsClientArgError("pseudonym must be $PSEUDONYM_LEN bytes")
        }
        val endpoint = "/ds/mailbox/${pseudonymHex(pseudonym)}/reach"
        val body = json.encodeToString(
            ReachWireRequest.serializer(),
            ReachWireRequest(
                add = add.map { ReachWireAdd(reach_pk_b64 = base64Encode(it.publicKey), expires_at_ms = it.expiresAtMs) },
                remove = remove.map(::base64Encode),
                open = open,
            ),
        ).toByteArray(Charsets.UTF_8)
        suspend fun attempt(): HttpResponse = send(
            method = HttpMethod.Put,
            endpoint = endpoint,
            query = "",
            body = body,
            contentType = ContentType.Application.Json,
            waitSecs = null,
            signer = signer,
        )
        var response = attempt()
        if (response.status.value == HTTP_UNAUTHORIZED) {
            try {
                claimMailbox(pseudonym, signer)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (_: Throwable) {
                // The retry below reports the outcome.
            }
            response = attempt()
        }
        if (!response.status.isSuccess()) {
            throw proofOrHttpError(response, endpoint)
        }
    }

    /**
     * `GET /ds/mailbox/{hex}/reach` — the reach keys the owner registered on
     * its own mailbox (ADR-0060 section 1, "Versions after a restore"), signed
     * by the identity key the mailbox is named after ([signer], the mailbox
     * signer), exactly like the owner's own message read. Public keys only;
     * an expired key is not listed.
     *
     * Every refusal is the uniform `401`. A mailbox that was never claimed
     * answers that too, so on a `401` the mailbox is claimed (idempotent for
     * its own key) and the read sent once more; a second refusal throws, as
     * does any other non-2xx. A network failure is [DsNetworkError]. The
     * caller treats every throw as "unknown" and registers nothing it could
     * not prove current.
     */
    public suspend fun fetchMailboxReach(pseudonym: ByteArray, signer: DsJournalSigner): MailboxReachList {
        if (pseudonym.size != PSEUDONYM_LEN) {
            throw DsClientArgError("pseudonym must be $PSEUDONYM_LEN bytes")
        }
        val endpoint = "/ds/mailbox/${pseudonymHex(pseudonym)}/reach"
        suspend fun attempt(): HttpResponse = send(
            method = HttpMethod.Get,
            endpoint = endpoint,
            query = "",
            body = ByteArray(0),
            contentType = null,
            waitSecs = null,
            signer = signer,
        )
        var response = attempt()
        if (response.status.value == HTTP_UNAUTHORIZED) {
            try {
                claimMailbox(pseudonym, signer)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (_: Throwable) {
                // The retry below reports the outcome.
            }
            response = attempt()
        }
        if (!response.status.isSuccess()) {
            throw proofOrHttpError(response, endpoint)
        }
        val parsed: ReachListWireResponse = response.body()
        return MailboxReachList(
            keys = parsed.keys.map { MailboxReachKey(base64Decode(it.reach_pk_b64), it.expires_at_ms) },
            open = parsed.open,
        )
    }

    /**
     * `POST /ds/mailbox/{hex}/reach/check` — whether the reach key [reach]
     * signs with is registered and unexpired on [pseudonym]'s mailbox:
     * `true` on `204`, `false` on `401`. A searcher asks before showing a
     * result (ADR-0060 §2), so a narrowed setting takes effect at once.
     * Throws on anything else (a network failure is never "not found").
     */
    public suspend fun checkReach(pseudonym: ByteArray, reach: DsJournalSigner): Boolean {
        if (pseudonym.size != PSEUDONYM_LEN) {
            throw DsClientArgError("pseudonym must be $PSEUDONYM_LEN bytes")
        }
        val endpoint = "/ds/mailbox/${pseudonymHex(pseudonym)}/reach/check"
        val response = send(
            method = HttpMethod.Post,
            endpoint = endpoint,
            query = "",
            body = ByteArray(0),
            contentType = ContentType.Application.OctetStream,
            waitSecs = null,
            signer = reach,
        )
        return when {
            response.status.isSuccess() -> true
            response.status.value == HTTP_UNAUTHORIZED -> false
            else -> throw DsHttpError(response.status.value, endpoint, response.safeReadBody())
        }
    }

    // -----------------------------------------------------------------
    // ADR-0053 §4: the knock
    // -----------------------------------------------------------------

    /**
     * `POST /ds/group/{hex}/knock` spending one `ds-knock` token: [payloads]
     * are the rows (at most four, 16 KiB in total); an EMPTY list is a probe
     * that stores nothing and only learns the group's epoch. A token refusal
     * is retried once with another token.
     */
    public suspend fun knock(
        groupId: ByteArray,
        payloads: List<ByteArray>,
    ): KnockResult {
        if (groupId.isEmpty()) {
            throw DsClientArgError("groupId must be non-empty")
        }
        val endpoint = "/ds/group/${pseudonymHex(groupId)}/knock"
        val body = json.encodeToString(
            KnockWireRequest.serializer(),
            KnockWireRequest(payloads_b64 = payloads.map(::base64Encode)),
        ).toByteArray(Charsets.UTF_8)
        val response = anonTokens.sendWithToken(AnonTokenContext.DS_KNOCK) { name, value ->
            try {
                anonClient.request("$baseUrl$endpoint") {
                    method = HttpMethod.Post
                    header(name, value)
                    contentType(ContentType.Application.Json)
                    setBody(body)
                }
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (cause: Throwable) {
                throw DsNetworkError(endpoint, cause)
            }
        }
        if (!response.status.isSuccess()) {
            throw DsHttpError(response.status.value, endpoint, response.safeReadBody())
        }
        val parsed: KnockWireResponse = response.body()
        return KnockResult(ids = parsed.ids, epoch = parsed.epoch?.toULong())
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
     * (a read that raced a lagging registration), then — for a journal this
     * account OWNS (ADR-0057) or a mailbox — once after a claim; a third
     * refusal surfaces as [DsProofRefusedError],
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
        wake: DsWake? = null,
    ): HttpResponse {
        val wakeHeader = wake?.headerValue()
        suspend fun attempt(signer: DsJournalSigner?): HttpResponse =
            send(method, endpoint, query, body, contentType, waitSecs, signer, wakeHeader)
        val first = journal?.let { it.explicit ?: signers.current(it.key) }
        var response = attempt(first)
        if (journal == null || response.status.value != HTTP_UNAUTHORIZED) return response
        if (first == null) throw refusedError(response, endpoint)

        val previous = if (journal.explicit == null) signers.previous(journal.key) else null
        if (previous != null && previous.epoch != first.epoch) {
            response = attempt(previous)
            if (response.status.value != HTTP_UNAUTHORIZED) return response
        }
        if (journal.reclaim) {
            // ADR-0057 — a group journal is re-claimed only by its OWNER (the
            // source installs the hook for an owned journal only), with the
            // root; a plain member waits for the committer's registration and
            // retries on its next poll. A mailbox keeps the ADR-0053 re-claim.
            val reclaimed = try {
                when (journal.kind) {
                    Journal.Kind.GROUP -> signers.ownerClaim(journal.key)?.invoke() ?: false
                    Journal.Kind.MAILBOX -> {
                        claimMailbox(journal.pseudonym, first)
                        true
                    }
                }
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (_: Throwable) {
                false
            }
            if (reclaimed) {
                val again = journal.explicit ?: signers.current(journal.key) ?: first
                response = attempt(again)
                if (response.status.value != HTTP_UNAUTHORIZED) return response
            }
        }
        val refused = refusedError(response, endpoint)
        // ADR-0057 second security pass, finding 1 — refused at this device's
        // own epoch after everything else was tried: an owner or admin
        // schedules the repair check (a member's note is dropped there). A
        // wrong device clock is not a journal defect and notes nothing.
        if (journal.kind == Journal.Kind.GROUP && !refused.clockSkewed) {
            val epoch = (signers.current(journal.key) ?: first).epoch
            signers.refusedAtHead(journal.key, epoch)
        }
        throw refused
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
        wakeHeader: String? = null,
        extraHeaders: Map<String, String> = emptyMap(),
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
                if (wakeHeader != null) header(DS_WAKE_HEADER, wakeHeader)
                for ((name, value) in extraHeaders) header(name, value)
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

        private const val HTTP_OK: Int = 200

        private const val HTTP_CREATED: Int = 201

        /** ADR-0057 — a statement is tried at most twice: once, and once
         *  more after a `409` (a lock race the retry wins). */
        private const val STATEMENT_ATTEMPTS: Int = 2

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
    val latest_epoch: Long? = null,
    /** ADR-0091 — `[{epoch, id}]`, see [FetchResult.registeredCommits]. */
    val commits: List<RegisteredCommitWire>? = null,
)

@Serializable
private data class RegisteredCommitWire(
    val epoch: Long,
    val id: Long,
)

@Serializable
@Suppress("ConstructorParameterNaming")
private data class FetchWireMessage(
    val id: Long,
    val payload_b64: String,
    val created_at: String,
    /** ADR-0060 (2026-11-06) — on the owner's signed mailbox read, the reach
     *  key that admitted the row; absent on a proofless row. */
    val reach_pk_b64: String? = null,
)

@Serializable
@Suppress("ConstructorParameterNaming")
private data class KnockWireRequest(
    val payloads_b64: List<String>,
)

@Serializable
@Suppress("ConstructorParameterNaming")
private data class ReachWireAdd(
    val reach_pk_b64: String,
    val expires_at_ms: Long? = null,
)

@Serializable
private data class ReachWireRequest(
    val add: List<ReachWireAdd>,
    val remove: List<String>,
    val open: Boolean? = null,
)

@Serializable
@Suppress("ConstructorParameterNaming")
private data class ReachListWireEntry(
    val reach_pk_b64: String,
    val expires_at_ms: Long? = null,
)

@Serializable
private data class ReachListWireResponse(
    val keys: List<ReachListWireEntry> = emptyList(),
    val open: Boolean = false,
)

/** One reach key registered on the owner's mailbox, as `GET .../reach` lists
 *  it. */
public class MailboxReachKey(public val publicKey: ByteArray, public val expiresAtMs: Long?)

/** What `GET /ds/mailbox/{hex}/reach` answered. */
public class MailboxReachList(public val keys: List<MailboxReachKey>, public val open: Boolean)

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
    /** ADR-0060 (2026-11-06) — the reach key that admitted a mailbox row (the
     *  owner's read only); `null` on a proofless row and on journals. */
    val reachPk: ByteArray? = null,
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
    /** The newest epoch registered on the DS for a CLAIMED group journal,
     *  present on a signed read only; `null` for an unclaimed journal and
     *  for mailboxes. A DeviceGroup reader compares it with its own epoch to
     *  notice a fork it can never catch up from ([me.lakko.app.devices.sync.DeviceGroupFork]). */
    val latestEpoch: ULong? = null,
    /** ADR-0091 — a signed read of a CLAIMED group journal only: the commit
     *  row the DS registered for each epoch above the one the read was signed
     *  at (`commits`), keyed by the epoch the row moves the journal TO. Row
     *  `id` of entry `n` is THE commit from `n - 1` to `n` (ADR-0081); a
     *  commit-shaped row posted through the plain publish route is never
     *  named. `null` where [latestEpoch] is. */
    val registeredCommits: Map<ULong, Long>? = null,
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

/**
 * ADR-0060 §2 — a write to someone's mailbox whose reach proof the server did
 * not accept: the key it was signed with is not (or no longer) registered.
 * The person changed who can find them, or revoked the link.
 */
public class DsReachRefusedError(
    endpoint: String,
    body: String,
) : DsHttpError(status = 401, endpoint = endpoint, body = body, message = "ds $endpoint → 401: reach refused")

/**
 * ADR-0060 §2 — what authorises one write into someone else's mailbox: a
 * reach-key signer (`DsProofSigner.forReach(seed)`), and, for a delegated
 * write, the delegation header value naming [signer]'s key.
 */
public class DsReachAuth(
    public val signer: DsJournalSigner,
    public val delegationHeader: String? = null,
)

/** One reach public key to register, with its optional expiry. */
public class DsReachKeyAdd(
    public val publicKey: ByteArray,
    public val expiresAtMs: Long? = null,
)

/** ADR-0081 decision 1 — what the commit route answered: the row's id and
 *  time, and whether it was the identical re-send of a commit that had
 *  already landed (`200`) rather than a new row (`201`). */
public data class DsCommitAnswer(
    val id: Long,
    val createdAt: String,
    val resent: Boolean,
)

/** ADR-0081 — the commit route's `409`: another commit owns the epoch, so
 *  nothing of this commit is on the DS. */
public class DsCommitConflictError(
    endpoint: String,
    body: String,
) : DsHttpError(status = 409, endpoint = endpoint, body = body, message = "ds $endpoint → 409: epoch taken")

/** Network-level failure (DNS, TLS, no route, …). */
public class DsNetworkError(
    public val endpoint: String,
    cause: Throwable,
) : RuntimeException("ds $endpoint network error: $cause", cause)
