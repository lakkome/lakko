// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import me.lakko.app.transport.lakkoHttpEngine
import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.request.header
import io.ktor.client.request.request
import io.ktor.http.HttpMethod
import io.ktor.http.content.ByteArrayContent
import me.lakko.app.anon.AnonTokenContext
import me.lakko.app.anon.AnonTokenSource
import me.lakko.app.anon.AnonTokens
import me.lakko.app.anon.AnonymousHttp
import me.lakko.app.anon.sendWithToken
import uniffi.lakko_ffi_uniffi.ChannelOwnerSigner
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.channelOwnerProofHeaderName
import uniffi.lakko_ffi_uniffi.channelOwnerSigner
import uniffi.lakko_ffi_uniffi.channelCopublisherPath
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.config.ApiService
import io.ktor.client.plugins.contentnegotiation.ContentNegotiation
import io.ktor.client.request.delete
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.parameter
import io.ktor.client.request.post
import io.ktor.client.request.put
import io.ktor.client.request.setBody
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import io.ktor.serialization.kotlinx.json.json
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * Ktor-based client for `lakko-svc-channels` (Phase 4
 * G4-2 Android port).
 *
 * Mirror of `apps/web/src/channels/api.ts` and
 * `apps/ios/Lakko/Channels/ChannelsClient.swift`. Same
 * endpoints, same stable error codes, JS-camelCase ↔
 * Kotlin-camelCase names line up trivially.
 *
 * ## Default base URL
 *
 * `http://10.0.2.2:8086` — Android-emulator loopback
 * alias + the services/channels dev bind. Override on
 * physical devices (LAN IP) and in production
 * (Cloudflare tunnel URL).
 */
public class ChannelsClient(
    public val baseUrl: String = DEFAULT_BASE_URL,
    engine: HttpClientEngine? = null,
    /** Engine for subscribe (a token route). Production:
     *  [AnonymousHttp.engine]; tests: `null` reuses [engine]. */
    anonEngine: HttpClientEngine? = null,
    /** Where subscribe draws its `channel-subscribe` token. */
    private val anonTokens: AnonTokenSource = AnonTokens.shared,
    /** The publisher role per (account, channel) the signed writes use
     *  (ADR-0059 section 9). Production: the process-wide cache. */
    private val publisherRoles: ChannelPublisherRoles = ChannelPublisherRoles.shared,
) : AutoCloseable {

    private val jsonCodec = Json {
        ignoreUnknownKeys = true
        explicitNulls = false
    }

    private val client: HttpClient = if (engine != null) {
        HttpClient(engine) { install(ContentNegotiation) { json(jsonCodec) } }
    } else {
        HttpClient(lakkoHttpEngine()) { install(ContentNegotiation) { json(jsonCodec) } }
    }

    /** Subscribe's client: no session, its own pool. */
    private val anonClient: HttpClient =
        HttpClient(anonEngine ?: engine ?: AnonymousHttp.engine) {
            install(ContentNegotiation) { json(jsonCodec) }
        }

    override fun close() {
        client.close()
        anonClient.close()
    }

    // -----------------------------------------------------------------
    // Channels
    // -----------------------------------------------------------------

    @Serializable
    private data class CreateReq(val channel_id: String, val owner_pubkey_hex: String, val private: Boolean = false)

    @Serializable
    private data class CreateResp(val channel_id: String)

    /** `POST /v1/channels` — register a fresh channel under the owner key
     *  [accountIdentity] derives for it (ADR-0059 section 1). The same key
     *  again is idempotent; another key for an existing id is refused with
     *  409 `channel-exists`. [owner] is [ownerSigner] of this channel. */
    public suspend fun createChannel(
        channelIdHex: String,
        owner: ChannelOwnerSigner,
        /** ADR-0087 section 7 — applied only when the channel is created. */
        private: Boolean = false,
    ): String {
        val ownerHex = owner.verifyingKey().joinToString("") { "%02x".format(it) }
        val response: HttpResponse = client.post("$baseUrl/v1/channels") {
            contentType(ContentType.Application.Json)
            setBody(CreateReq(channelIdHex, ownerHex, private))
        }
        if (!response.status.isSuccess()) {
            throw decodeError(response, "create channel")
        }
        publisherRoles.remember(ownerHex, ChannelPublisherRole.OWNER)
        return response.body<CreateResp>().channel_id
    }

    // -----------------------------------------------------------------
    // Posts
    // -----------------------------------------------------------------

    @Serializable
    private data class PostJson(
        val cursor: ULong,
        val server_ts_ms: ULong,
        val payload_b64: String,
    )

    @Serializable
    private data class FetchResp(
        val posts: List<PostJson>,
        val next_after: ULong,
    )

    /** `POST /v1/channels/:id/posts` — publish opaque ChannelPost bytes,
     *  signed with this account's publisher role (ADR-0059 section 9) over
     *  exactly those bytes. A refused proof throws [ChannelsApiException]
     *  with code [PUBLISHER_REFUSED]. */
    public suspend fun publishPost(
        channelIdHex: String,
        payload: ByteArray,
        proofs: ChannelPublisherProofs,
    ): PostView {
        val response = sendAsPublisher(
            proofs,
            HttpMethod.Post,
            "/v1/channels/$channelIdHex/posts",
            "",
            payload,
            ContentType.Application.OctetStream,
            "publish post",
        )
        val body = response.body<PostJson>()
        return PostView(body.cursor, body.server_ts_ms, body.payload_b64)
    }

    /** `GET /v1/channels/:id/posts?after=…&limit=…` — cursor-paginated fetch. */
    public suspend fun fetchPosts(
        channelIdHex: String,
        after: ULong,
        limit: Int? = null,
    ): FetchPostsResult {
        val response: HttpResponse = client.get(
            "$baseUrl/v1/channels/$channelIdHex/posts"
        ) {
            parameter("after", after.toString())
            if (limit != null) parameter("limit", limit.toString())
        }
        if (!response.status.isSuccess()) {
            throw decodeError(response, "fetch posts")
        }
        val body = response.body<FetchResp>()
        return FetchPostsResult(
            posts = body.posts.map {
                PostView(it.cursor, it.server_ts_ms, it.payload_b64)
            },
            nextAfter = body.next_after,
        )
    }

    @Serializable
    private data class EpochResp(val epoch: UInt)

    /** `GET /v1/channels/:id/epoch`. */
    public suspend fun fetchEpoch(channelIdHex: String): UInt {
        val response: HttpResponse = client.get(
            "$baseUrl/v1/channels/$channelIdHex/epoch"
        )
        if (!response.status.isSuccess()) {
            throw decodeError(response, "fetch epoch")
        }
        return response.body<EpochResp>().epoch
    }

    // -----------------------------------------------------------------
    // Subscribers (G4-3c)
    // -----------------------------------------------------------------

    @Serializable
    private data class SubscribeReq(val subscriber_pubkey_hex: String)

    @Serializable
    private data class SubscribeResp(val subscriber_id: String)

    @Serializable
    private data class SubscribersResp(val subscribers: List<String>)

    @Serializable
    private data class EnvelopeResp(val envelope_b64: String)

    @Serializable
    private data class EnvelopeEntryJson(val epoch: UInt, val envelope_b64: String)

    @Serializable
    private data class EnvelopesResp(val envelopes: List<EnvelopeEntryJson>)

    /** `POST /v1/channels/:id/subscribers` — register a
     *  subscriber's X25519 public key. Spends one `channel-subscribe`
     *  anonymous token (ADR-0059 section 4) on the anonymous client; the
     *  request carries no session. */
    public suspend fun subscribe(
        channelIdHex: String,
        subscriberPubkeyHex: String,
    ): String {
        val url = "$baseUrl/v1/channels/$channelIdHex/subscribers"
        val response: HttpResponse = anonTokens.sendWithToken(AnonTokenContext.CHANNEL_SUBSCRIBE) { name, value ->
            anonClient.request(url) {
                method = HttpMethod.Post
                header(name, value)
                contentType(ContentType.Application.Json)
                setBody(SubscribeReq(subscriberPubkeyHex))
            }
        }
        if (!response.status.isSuccess()) {
            throw decodeError(response, "subscribe")
        }
        return response.body<SubscribeResp>().subscriber_id
    }

    /**
     * `DELETE /v1/channels/:id/subscribers/:pk` — leave the channel
     * (ADR-0060 §3): the server drops the subscriber row, its seed envelopes
     * and its member card. Authenticated by [signatureHeaderName]
     * (`unsubscribeSigHeaderName()`) carrying [signatureHeaderValue]
     * (`ChannelSubscriberKeypair.unsubscribeRequestHeader`), signed by the
     * subscriber key the path names. A `404` (channel or subscriber already
     * gone) counts as done; any other non-2xx throws.
     */
    public suspend fun unsubscribe(
        channelIdHex: String,
        subscriberPubkeyHex: String,
        signatureHeaderName: String,
        signatureHeaderValue: String,
    ) {
        val response: HttpResponse = client.delete(
            "$baseUrl/v1/channels/$channelIdHex/subscribers/$subscriberPubkeyHex"
        ) {
            header(signatureHeaderName, signatureHeaderValue)
        }
        if (response.status.isSuccess() || response.status.value == 404) return
        throw decodeError(response, "unsubscribe")
    }

    /** `GET /v1/channels/:id/subscribers` — list registered
     *  subscriber pubkey-hex strings. Used by the publisher
     *  to fan out HPKE-sealed seed envelopes. Owner-signed (ADR-0059
     *  section 4) with [owner] ([ownerSigner] of this channel), over the
     *  service path (never the `/channels` prefix nginx strips). */
    public suspend fun listSubscribers(channelIdHex: String, owner: ChannelOwnerSigner): List<String> {
        val path = "/v1/channels/$channelIdHex/subscribers"
        val proof = owner.sign("GET", path, "", ByteArray(0))
        val response: HttpResponse = client.get("$baseUrl$path") {
            header(channelOwnerProofHeaderName(), proof)
        }
        if (!response.status.isSuccess()) {
            throw decodeError(response, "list subscribers")
        }
        return response.body<SubscribersResp>().subscribers
    }

    /** The subscriber list for a publisher of the channel, owner or
     *  co-publisher, signed with the role this account holds (ADR-0059
     *  sections 4 and 9): see [sendAsPublisher]. A co-publisher the owner has
     *  not appointed yet gets the 401. */
    public suspend fun listSubscribersAsPublisher(
        channelIdHex: String,
        proofs: ChannelPublisherProofs,
    ): List<String> {
        val response = sendAsPublisher(
            proofs,
            HttpMethod.Get,
            "/v1/channels/$channelIdHex/subscribers",
            "",
            ByteArray(0),
            null,
            "list subscribers",
        )
        return response.body<SubscribersResp>().subscribers
    }

    /** [listSubscribersAsPublisher] with the signers derived from
     *  [accountIdentity] for this call. */
    public suspend fun listSubscribersAsPublisher(
        channelIdHex: String,
        accountIdentity: Identity,
    ): List<String> = ChannelPublisherSigners.derive(accountIdentity, channelIdHex).use {
        listSubscribersAsPublisher(channelIdHex, it)
    }

    /** `PUT` (appoint) / `DELETE` (revoke) `/v1/channels/:id/copublishers/:key`
     *  — owner-signed (ADR-0059 section 1). Idempotent both ways; 409
     *  `too-many-copublishers` past the cap. [copublisherKey] must come from a
     *  binding verified against the joiner key the owner approved, never from
     *  the server. */
    public suspend fun setCoPublisherAppointment(
        appoint: Boolean,
        channelIdHex: String,
        accountIdentity: Identity,
        copublisherKey: ByteArray,
    ) {
        val method = if (appoint) HttpMethod.Put else HttpMethod.Delete
        val path = channelCopublisherPath(channelIdBytes(channelIdHex), copublisherKey)
        val proof = ownerSigner(accountIdentity, channelIdHex).use {
            it.sign(method.value, path, "", ByteArray(0))
        }
        val response: HttpResponse = client.request("$baseUrl$path") {
            this.method = method
            header(channelOwnerProofHeaderName(), proof)
        }
        if (!response.status.isSuccess()) {
            throw decodeError(response, "co-publisher appointment")
        }
    }

    // -----------------------------------------------------------------
    // ADR-0059 — the directory (Discover)
    // -----------------------------------------------------------------

    @Serializable
    private data class DirectoryItems(val items: List<me.lakko.app.discover.DirectoryItemWire> = emptyList())

    /** Whether [owner] is the channel's OWNER key (not a co-publisher's): an
     *  owner-signed read of the subscriber list is accepted only under the key
     *  registered at creation. `null` when the server could not answer. */
    public suspend fun probeChannelOwner(channelIdHex: String, owner: ChannelOwnerSigner): Boolean? =
        try {
            listSubscribers(channelIdHex, owner)
            true
        } catch (e: ChannelsApiException) {
            if (e.status == 401) false else null
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (_: Throwable) {
            null
        }

    /** `PUT /v1/channels/:id/listing` — store or replace the listing
     *  (owner-signed; [listing] is the canonical `composeChannelListing` bytes). */
    public suspend fun putListing(channelIdHex: String, owner: ChannelOwnerSigner, listing: ByteArray) {
        val path = "/v1/channels/$channelIdHex/listing"
        val proof = owner.sign("PUT", path, "", listing)
        val response: HttpResponse = client.request("$baseUrl$path") {
            method = HttpMethod.Put
            header(channelOwnerProofHeaderName(), proof)
            contentType(ContentType.Application.OctetStream)
            setBody(listing)
        }
        if (!response.status.isSuccess()) throw decodeError(response, "put listing")
    }

    /**
     * `PUT /v1/channels/:id/owner` (ADR-0074 section 3.5, "Channels") -
     * replace the channel's owner key with [newOwnerKey] (32 raw bytes, the
     * body). [signer] is the CURRENT owner's signer: the old seed's for the
     * handover, the new seed's for the probe that asks whether it already
     * landed (naming the key that already owns the channel is a `204` that
     * changes nothing). Throws [ChannelsApiException]: `401
     * owner-proof-required` (the signer is not the owner), `400
     * bad-owner-pubkey`, `409 owner-changed`.
     */
    public suspend fun replaceOwner(channelIdHex: String, signer: ChannelOwnerSigner, newOwnerKey: ByteArray) {
        val path = "/v1/channels/$channelIdHex/owner"
        val proof = signer.sign("PUT", path, "", newOwnerKey)
        val response: HttpResponse = client.request("$baseUrl$path") {
            method = HttpMethod.Put
            header(channelOwnerProofHeaderName(), proof)
            contentType(ContentType.Application.OctetStream)
            setBody(newOwnerKey)
        }
        if (!response.status.isSuccess()) throw decodeError(response, "replace owner")
    }

    /** `DELETE /v1/channels/:id/listing` — withdraw the listing (owner-signed,
     *  idempotent). */
    public suspend fun deleteListing(channelIdHex: String, owner: ChannelOwnerSigner) {
        val path = "/v1/channels/$channelIdHex/listing"
        val proof = owner.sign("DELETE", path, "", ByteArray(0))
        val response: HttpResponse = client.request("$baseUrl$path") {
            method = HttpMethod.Delete
            header(channelOwnerProofHeaderName(), proof)
        }
        if (!response.status.isSuccess()) throw decodeError(response, "delete listing")
    }

    /** One channel's listing, or `null` when it is not listed (404). No
     *  session, no token. */
    public suspend fun fetchListing(channelIdHex: String): me.lakko.app.discover.DirectoryItemWire? {
        val response: HttpResponse = anonClient.get("$baseUrl/v1/directory/listing/$channelIdHex")
        if (response.status.value == 404) return null
        if (!response.status.isSuccess()) throw decodeError(response, "fetch listing")
        return response.body<me.lakko.app.discover.DirectoryItemWire>()
    }

    /** The top listings of one (topic, language) bucket. No session, no token. */
    public suspend fun fetchBucket(topic: Int, language: String): List<me.lakko.app.discover.DirectoryItemWire> {
        val response: HttpResponse = anonClient.get("$baseUrl/v1/directory/bucket/$topic/$language")
        if (!response.status.isSuccess()) throw decodeError(response, "fetch bucket")
        return response.body<DirectoryItems>().items
    }

    /** Search the directory: one `directory-search` token; the query is sent. */
    public suspend fun searchDirectory(query: String): List<me.lakko.app.discover.DirectoryItemWire> {
        val url = "$baseUrl/v1/directory/search"
        val response: HttpResponse = anonTokens.sendWithToken(AnonTokenContext.DIRECTORY_SEARCH) { name, value ->
            anonClient.request(url) {
                method = HttpMethod.Get
                header(name, value)
                parameter("q", query)
            }
        }
        if (!response.status.isSuccess()) throw decodeError(response, "search directory")
        return response.body<DirectoryItems>().items
    }

    /** `GET /v1/channels/:id/subscribers/:pk/envelope` —
     *  fetch the latest seed envelope. Returns `null` when
     *  registered but no envelope yet (server returns 404
     *  `envelope-not-ready` which we map to null so the
     *  caller can branch cleanly). */
    public suspend fun fetchEnvelope(
        channelIdHex: String,
        subscriberPubkeyHex: String,
    ): String? {
        val response: HttpResponse = client.get(
            "$baseUrl/v1/channels/$channelIdHex/subscribers/$subscriberPubkeyHex/envelope"
        )
        if (!response.status.isSuccess()) {
            val err = decodeError(response, "fetch envelope")
            if (err.code == "envelope-not-ready") {
                return null
            }
            throw err
        }
        return response.body<EnvelopeResp>().envelope_b64
    }

    /** ADR-0031 — `GET /v1/channels/:id/subscribers/:pk/envelopes`
     *  (plural). Fetch the **retained set** — the most-recent
     *  N (default 4) seed envelopes kept for this subscriber,
     *  ascending by epoch — so a subscriber that missed one or
     *  more rotations can open every retained epoch's seed and
     *  decrypt each post with the seed matching ITS epoch.
     *
     *  Returns `(epoch, envelope)` pairs with each envelope
     *  already base64url-decoded to raw wire bytes, ascending
     *  by epoch. An empty list when the subscriber is
     *  registered but no envelope has landed yet (server
     *  returns 404 `envelope-not-ready`, mapped to empty so
     *  the caller falls back to the singular endpoint / poll).
     *  An entry whose `envelope_b64` fails to decode is dropped
     *  rather than aborting the whole set. */
    public suspend fun fetchEnvelopes(
        channelIdHex: String,
        subscriberPubkeyHex: String,
    ): List<Pair<UInt, ByteArray>> {
        val response: HttpResponse = client.get(
            "$baseUrl/v1/channels/$channelIdHex/subscribers/$subscriberPubkeyHex/envelopes"
        )
        if (!response.status.isSuccess()) {
            val err = decodeError(response, "fetch envelopes")
            if (err.code == "envelope-not-ready") {
                return emptyList()
            }
            throw err
        }
        return response.body<EnvelopesResp>().envelopes
            .sortedBy { it.epoch }
            .mapNotNull { entry ->
                val bytes = ChannelsComposer.base64UrlDecodeOrNull(entry.envelope_b64)
                    ?: return@mapNotNull null
                entry.epoch to bytes
            }
    }

    /** `POST /v1/channels/:id/subscribers/:pk/envelope` —
     *  publisher delivers a per-subscriber HPKE-sealed seed
     *  envelope. The server takes raw `Bytes` — same opacity
     *  contract as the publish endpoint.
     *
     *  Throws `ChannelsApiException`:
     *  - `unknown-subscriber` (404) — pubkey not registered
     *  - `bad-envelope-len`   (400) — wrong wire length
     *  - `bad-channel-id`     (400) — wrong channel-id shape
     *  - `bad-subscriber-id`  (400) — wrong pubkey-hex shape
     *  - `envelope-epoch-in-future` (409) — [epoch] is greater
     *    than the channel's current epoch (ADR-0031 — a
     *    publisher may only file at an epoch it has already
     *    committed to; the staged/commit path handles the next
     *    epoch)
     *
     *  ADR-0031: [epoch] is the concrete epoch the publisher
     *  sealed this envelope under. When non-null it is appended
     *  as `?epoch=`; the server files the envelope at that
     *  epoch and prunes the subscriber to its highest N. Absent
     *  (null) → the server files at its current epoch (the
     *  backward-compatible shape).
     *
     *  Publisher-signed (ADR-0059 section 9) over the path, the
     *  `epoch=N` query and the envelope bytes. */
    public suspend fun deliverEnvelope(
        channelIdHex: String,
        subscriberPubkeyHex: String,
        envelope: ByteArray,
        proofs: ChannelPublisherProofs,
        epoch: Int? = null,
    ) {
        // The query is part of what the proof covers: built once, signed and
        // sent as is.
        sendAsPublisher(
            proofs,
            HttpMethod.Post,
            "/v1/channels/$channelIdHex/subscribers/$subscriberPubkeyHex/envelope",
            if (epoch != null) "epoch=$epoch" else "",
            envelope,
            ContentType.Application.OctetStream,
            "deliver envelope",
        )
    }

    /** G4-2-rotation-timer-android — atomic batch rotation
     *  fanout. The publisher walks `listSubscribers(...)`,
     *  reseals the new MLS-exporter seed once per
     *  subscriber via `sealSubscriberSeedEnvelope`, then
     *  posts the batch here. The server commits or rejects
     *  the whole batch atomically (G4-4 / C-42); on
     *  success the response carries the new epoch id,
     *  which the caller records as part of the rotation
     *  state.
     *
     *  Throws `ChannelsApiException`:
     *  - `unknown-subscriber` (404) — one of the pubkeys
     *    is not registered (batch aborted; no envelopes
     *    landed)
     *  - `bad-envelope-len`   (400) — wrong wire length
     *  - `bad-channel-id`     (400) — wrong channel-id shape
     *  - `bad-subscriber-id`  (400) — wrong pubkey-hex shape */
    public suspend fun rotateChannel(
        channelIdHex: String,
        envelopes: List<RotateBatchEntry>,
        proofs: ChannelPublisherProofs,
    ): UInt {
        val response = sendAsPublisher(
            proofs,
            HttpMethod.Post,
            "/v1/channels/$channelIdHex/rotate",
            "",
            jsonBytes(jsonCodec.encodeToString(RotateRequest.serializer(), RotateRequest(envelopes))),
            ContentType.Application.Json,
            "rotate channel",
        )
        return response.body<EpochView>().epoch
    }

    /** ADR-0030 Sprint D2 — staged-rotation PREPARE phase.
     *  Uploads one bounded chunk of the next epoch's
     *  envelopes to the staging area WITHOUT bumping the
     *  epoch or serving them to subscribers. Staged rows are
     *  keyed on `(channel, target_epoch, subscriber)`, so
     *  re-posting a chunk is an idempotent upsert — a client
     *  that dropped mid-fanout simply re-sends the missing
     *  chunks (resumability).
     *
     *  A chunk must carry at most [MAX_STAGE_CHUNK_ENTRIES]
     *  entries; the server refuses a larger chunk with 413
     *  `chunk-too-large`, so the caller splits its fanout.
     *
     *  Throws `ChannelsApiException`:
     *  - `chunk-too-large`    (413) — over the entry cap
     *  - `stale-target-epoch` (409) — targetEpoch != current + 1
     *  - `unknown-subscriber` (409) — a pubkey is not registered
     *  - `unknown-channel`    (404) — no such channel
     *  - `bad-envelope-len`   (400) — wrong wire length
     *  - `bad-channel-id`     (400) — wrong channel-id shape */
    public suspend fun stageRotation(
        channelIdHex: String,
        targetEpoch: UInt,
        envelopes: List<RotateBatchEntry>,
        proofs: ChannelPublisherProofs,
    ) {
        sendAsPublisher(
            proofs,
            HttpMethod.Post,
            "/v1/channels/$channelIdHex/rotate/stage",
            "",
            jsonBytes(
                jsonCodec.encodeToString(
                    StageRotateRequest.serializer(),
                    StageRotateRequest(targetEpoch, envelopes),
                ),
            ),
            ContentType.Application.Json,
            "stage rotation",
        )
    }

    /** ADR-0030 Sprint D2 — staged-rotation COMMIT phase, the
     *  single atomic point. Promotes the staged envelopes for
     *  `targetEpoch` into the live set + bumps the epoch in
     *  one transaction, then returns the new epoch id. C-42 is
     *  preserved: subscribers see the old epoch until this
     *  runs, then the new epoch with every staged envelope
     *  already live.
     *
     *  Throws `ChannelsApiException`:
     *  - `stale-target-epoch` (409) — targetEpoch != current + 1
     *  - `unknown-channel`    (404) — no such channel
     *  - `bad-channel-id`     (400) — wrong channel-id shape */
    public suspend fun commitRotation(
        channelIdHex: String,
        targetEpoch: UInt,
        proofs: ChannelPublisherProofs,
    ): UInt {
        val response = sendAsPublisher(
            proofs,
            HttpMethod.Post,
            "/v1/channels/$channelIdHex/rotate/commit",
            "",
            jsonBytes(
                jsonCodec.encodeToString(CommitRotateRequest.serializer(), CommitRotateRequest(targetEpoch)),
            ),
            ContentType.Application.Json,
            "commit rotation",
        )
        return response.body<EpochView>().epoch
    }

    @Serializable
    private data class RotateRequest(val envelopes: List<RotateBatchEntry>)

    @Serializable
    private data class StageRotateRequest(
        val target_epoch: UInt,
        val envelopes: List<RotateBatchEntry>,
    )

    @Serializable
    private data class CommitRotateRequest(val target_epoch: UInt)

    @Serializable
    private data class EpochView(val epoch: UInt)

    // -----------------------------------------------------------------
    // ADR-0059 section 9 — publisher-signed requests
    // -----------------------------------------------------------------

    /** UTF-8 bytes of a JSON body serialized once: these exact bytes are
     *  both signed and sent. */
    private fun jsonBytes(json: String): ByteArray = json.toByteArray(Charsets.UTF_8)

    /**
     * Send one publisher-signed request and return the successful response;
     * a refusal throws [ChannelsApiException].
     *
     * [path] is the service path (`/v1/channels/...`), which is what the
     * proof covers; the URL is [baseUrl] (which may carry the `/channels`
     * prefix nginx strips) + [path] + `?`[query]. [body] is sent byte for
     * byte as signed. Every attempt signs afresh (single-use nonces).
     *
     * With the role cached: signed once; a 401 forgets the role and
     * surfaces. With the role unknown: owner proof first, and on a 401 the
     * co-publisher proof; whichever is accepted is remembered.
     */
    internal suspend fun sendAsPublisher(
        proofs: ChannelPublisherProofs,
        method: HttpMethod,
        path: String,
        query: String,
        body: ByteArray,
        bodyType: ContentType?,
        action: String,
    ): HttpResponse {
        val url = if (query.isEmpty()) "$baseUrl$path" else "$baseUrl$path?$query"
        suspend fun attempt(role: ChannelPublisherRole): HttpResponse {
            val (name, value) = proofs.proofHeader(role, method.value, path, query, body)
            return client.request(url) {
                this.method = method
                header(name, value)
                if (bodyType != null) setBody(ByteArrayContent(body, bodyType))
            }
        }
        val key = proofs.roleKey
        val known = publisherRoles[key]
        if (known != null) {
            val response = attempt(known)
            if (response.status.isSuccess()) return response
            if (response.status.value == 401) publisherRoles.forget(key)
            throw decodeError(response, action)
        }
        val asOwner = attempt(ChannelPublisherRole.OWNER)
        if (asOwner.status.isSuccess()) {
            publisherRoles.remember(key, ChannelPublisherRole.OWNER)
            return asOwner
        }
        if (asOwner.status.value != 401) throw decodeError(asOwner, action)
        val asCopublisher = attempt(ChannelPublisherRole.COPUBLISHER)
        if (asCopublisher.status.isSuccess()) {
            publisherRoles.remember(key, ChannelPublisherRole.COPUBLISHER)
            return asCopublisher
        }
        throw decodeError(asCopublisher, action)
    }

    // -----------------------------------------------------------------
    // ADR-0087 phase B — public seeds, the flag, the count, removal,
    // invite keys, reactions and comments
    // -----------------------------------------------------------------

    @Serializable
    private data class AccessReq(val private: Boolean)

    /** `PUT /v1/channels/:id/access` — the channel's public / private flag
     *  (owner-signed, section 8). */
    public suspend fun setAccess(channelIdHex: String, owner: ChannelOwnerSigner, private: Boolean) {
        val path = "/v1/channels/$channelIdHex/access"
        val body = jsonCodec.encodeToString(AccessReq.serializer(), AccessReq(private)).toByteArray(Charsets.UTF_8)
        val proof = owner.sign("PUT", path, "", body)
        val response: HttpResponse = client.request("$baseUrl$path") {
            method = HttpMethod.Put
            header(channelOwnerProofHeaderName(), proof)
            setBody(ByteArrayContent(body, ContentType.Application.Json))
        }
        if (!response.status.isSuccess()) throw decodeError(response, "set access")
    }

    /** `PUT /v1/channels/:id/public-seed/:epoch` — file a public channel's
     *  seed envelope for [epoch] (section 6), publisher-signed. Immutable: the
     *  identical envelope again is a success. */
    public suspend fun putPublicSeed(
        channelIdHex: String,
        epoch: UInt,
        envelope: ByteArray,
        proofs: ChannelPublisherProofs,
    ) {
        sendAsPublisher(
            proofs,
            HttpMethod.Put,
            "/v1/channels/$channelIdHex/public-seed/$epoch",
            "",
            envelope,
            ContentType.Application.OctetStream,
            "put public seed",
        )
    }

    /** `GET /v1/channels/:id/public-seed/:epoch` — the envelope, or `null`
     *  when none is filed (404). Open, no session. */
    public suspend fun fetchPublicSeed(channelIdHex: String, epoch: UInt): ByteArray? {
        val response: HttpResponse = anonClient.get("$baseUrl/v1/channels/$channelIdHex/public-seed/$epoch")
        if (response.status.value == 404) return null
        if (!response.status.isSuccess()) throw decodeError(response, "fetch public seed")
        return response.body<ByteArray>()
    }

    @Serializable
    private data class CountResp(val count: Long)

    /** `GET /v1/channels/:id/subscribers/count` — the exact count, for the
     *  owner and publishers (section 15). */
    public suspend fun subscriberCount(channelIdHex: String, proofs: ChannelPublisherProofs): Long {
        val response = sendAsPublisher(
            proofs,
            HttpMethod.Get,
            "/v1/channels/$channelIdHex/subscribers/count",
            "",
            ByteArray(0),
            null,
            "subscriber count",
        )
        return response.body<CountResp>().count
    }

    /** `DELETE /v1/channels/:id/subscribers/:pk` by a publisher (section 7):
     *  removes a subscriber of a private channel; a rotation follows. */
    public suspend fun removeSubscriber(
        channelIdHex: String,
        subscriberPubkeyHex: String,
        proofs: ChannelPublisherProofs,
    ) {
        sendAsPublisher(
            proofs,
            HttpMethod.Delete,
            "/v1/channels/$channelIdHex/subscribers/$subscriberPubkeyHex",
            "",
            ByteArray(0),
            null,
            "remove subscriber",
        )
    }

    @Serializable
    private data class SubscriberRowJson(
        val seq: Long,
        val subscriber_id: String,
        val sealed_ticket_b64: String? = null,
    )

    @Serializable
    private data class SubscriberPageResp(
        val entries: List<SubscriberRowJson> = emptyList(),
        val next_after: Long = 0,
    )

    /** One page of subscriber rows with their sealed tickets (section 7),
     *  publisher-signed. */
    public suspend fun subscriberPage(
        channelIdHex: String,
        proofs: ChannelPublisherProofs,
        after: Long = 0,
        limit: Int = 500,
    ): SubscriberPage {
        val query = "after=$after&limit=$limit"
        val response = sendAsPublisher(
            proofs,
            HttpMethod.Get,
            "/v1/channels/$channelIdHex/subscribers",
            query,
            ByteArray(0),
            null,
            "list subscribers",
        )
        val body = response.body<SubscriberPageResp>()
        return SubscriberPage(
            rows = body.entries.map { e ->
                SubscriberRow(
                    seq = e.seq,
                    pubkeyHex = e.subscriber_id,
                    sealedTicket = e.sealed_ticket_b64?.let { ChannelsComposer.base64UrlDecodeOrNull(it) },
                )
            },
            nextAfter = body.next_after,
        )
    }

    @Serializable
    private data class InviteKeyJson(val invite_pk_hex: String)

    @Serializable
    private data class InviteKeysResp(val invite_keys: List<InviteKeyJson> = emptyList())

    /** `GET /v1/channels/:id/invite-keys` — the channel's registered invite
     *  keys (hex), publisher-signed. */
    public suspend fun listInviteKeys(channelIdHex: String, proofs: ChannelPublisherProofs): List<String> {
        val response = sendAsPublisher(
            proofs,
            HttpMethod.Get,
            "/v1/channels/$channelIdHex/invite-keys",
            "",
            ByteArray(0),
            null,
            "list invite keys",
        )
        return response.body<InviteKeysResp>().invite_keys.map { it.invite_pk_hex.lowercase() }
    }

    @Serializable
    private data class InviteKeyReq(val expiry_ms: Long, val max_uses: Long)

    /** `PUT /v1/channels/:id/invite-keys/:pk` (section 7): register a link's
     *  invite key with its expiry and use limit, publisher-signed. */
    public suspend fun putInviteKey(
        channelIdHex: String,
        invitePubkeyHex: String,
        expiryMs: Long,
        maxUses: Long,
        proofs: ChannelPublisherProofs,
    ) {
        val body = jsonCodec.encodeToString(InviteKeyReq.serializer(), InviteKeyReq(expiryMs, maxUses))
            .toByteArray(Charsets.UTF_8)
        sendAsPublisher(
            proofs,
            HttpMethod.Put,
            "/v1/channels/$channelIdHex/invite-keys/$invitePubkeyHex",
            "",
            body,
            ContentType.Application.Json,
            "put invite key",
        )
    }

    /** `DELETE /v1/channels/:id/invite-keys/:pk`: revoke, publisher-signed. */
    public suspend fun deleteInviteKey(channelIdHex: String, invitePubkeyHex: String, proofs: ChannelPublisherProofs) {
        sendAsPublisher(
            proofs,
            HttpMethod.Delete,
            "/v1/channels/$channelIdHex/invite-keys/$invitePubkeyHex",
            "",
            ByteArray(0),
            null,
            "delete invite key",
        )
    }

    @Serializable
    private data class SubscribeTicketReq(val subscriber_pubkey_hex: String, val sealed_ticket_b64: String)

    /** Subscribe to a PRIVATE channel (section 7): the sealed admission
     *  ticket in the body and the invite proof in its header; one
     *  `channel-subscribe` token. Every invite refusal is `401
     *  invite-proof-required`. */
    public suspend fun subscribeWithInvite(
        channelIdHex: String,
        subscriberPubkeyHex: String,
        sealedTicket: ByteArray,
        inviteProof: String,
    ): String {
        val url = "$baseUrl/v1/channels/$channelIdHex/subscribers"
        val ticketB64 = java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(sealedTicket)
        val response: HttpResponse = anonTokens.sendWithToken(AnonTokenContext.CHANNEL_SUBSCRIBE) { name, value ->
            anonClient.request(url) {
                method = HttpMethod.Post
                header(name, value)
                header(uniffi.lakko_ffi_uniffi.channelInviteProofHeaderName(), inviteProof)
                contentType(ContentType.Application.Json)
                setBody(SubscribeTicketReq(subscriberPubkeyHex, ticketB64))
            }
        }
        if (!response.status.isSuccess()) throw decodeError(response, "subscribe")
        return response.body<SubscribeResp>().subscriber_id
    }

    @Serializable
    private data class ReactionReq(val writer_key_hex: String, val label_hex: String)

    /** Add a reaction row (section 13a): the write proof, then one
     *  `channel-react` token. */
    public suspend fun addReaction(
        channelIdHex: String,
        postCursor: ULong,
        writerKeyHex: String,
        labelHex: String,
        writeProof: String,
    ) {
        val url = "$baseUrl/v1/channels/$channelIdHex/posts/$postCursor/reactions"
        val response: HttpResponse = anonTokens.sendWithToken(AnonTokenContext.CHANNEL_REACT) { name, value ->
            anonClient.request(url) {
                method = HttpMethod.Post
                header(name, value)
                header(uniffi.lakko_ffi_uniffi.channelWriteProofHeaderName(), writeProof)
                contentType(ContentType.Application.Json)
                setBody(ReactionReq(writerKeyHex, labelHex))
            }
        }
        if (!response.status.isSuccess()) throw decodeError(response, "add reaction")
    }

    /** Take a reaction off: a signed delete, no token. */
    public suspend fun deleteReaction(
        channelIdHex: String,
        postCursor: ULong,
        writerKeyHex: String,
        labelHex: String,
        writeProof: String,
    ) {
        val response: HttpResponse = anonClient.request(
            "$baseUrl/v1/channels/$channelIdHex/posts/$postCursor/reactions/$writerKeyHex/$labelHex",
        ) {
            method = HttpMethod.Delete
            header(uniffi.lakko_ffi_uniffi.channelWriteProofHeaderName(), writeProof)
        }
        if (!response.status.isSuccess() && response.status.value != 404) throw decodeError(response, "delete reaction")
    }

    @Serializable
    private data class LabelCountJson(val label_hex: String, val count: Long)

    @Serializable
    private data class PostCountsJson(
        val cursor: Long,
        val labels: List<LabelCountJson> = emptyList(),
        /** The post's maintained comment count (2026-11-05). */
        val comments: Long = 0,
    )

    @Serializable
    private data class CountsResp(val posts: List<PostCountsJson> = emptyList())

    /** What `GET .../reactions` says of the posts asked about: their label
     *  counts and their comment counts. */
    public class ReactionSummary(
        public val labels: Map<ULong, List<uniffi.lakko_ffi_uniffi.FfiReactionLabelCount>>,
        public val comments: Map<ULong, Long>,
    )

    /** Label counts of up to 50 posts, no session (cacheable). */
    public suspend fun reactionCounts(
        channelIdHex: String,
        cursors: List<ULong>,
    ): Map<ULong, List<uniffi.lakko_ffi_uniffi.FfiReactionLabelCount>> = reactionSummary(channelIdHex, cursors).labels

    /** Label counts and comment counts of up to 50 posts, no session. */
    public suspend fun reactionSummary(channelIdHex: String, cursors: List<ULong>): ReactionSummary {
        if (cursors.isEmpty()) return ReactionSummary(emptyMap(), emptyMap())
        val response: HttpResponse = anonClient.get("$baseUrl/v1/channels/$channelIdHex/reactions") {
            parameter("posts", cursors.take(50).joinToString(","))
        }
        if (!response.status.isSuccess()) throw decodeError(response, "reaction counts")
        val posts = response.body<CountsResp>().posts
        return ReactionSummary(
            labels = posts.associate { p ->
                p.cursor.toULong() to p.labels.mapNotNull { l ->
                    hexOrNull(l.label_hex)?.let { uniffi.lakko_ffi_uniffi.FfiReactionLabelCount(it, l.count.toULong()) }
                }
            },
            comments = posts.associate { it.cursor.toULong() to it.comments },
        )
    }

    /** Section 13a (phase B) — a reaction on a comment: the write proof, then
     *  one `channel-react` token. 201 added, 200 a repeat. */
    public suspend fun addCommentReaction(
        channelIdHex: String,
        postCursor: ULong,
        commentCursor: Long,
        writerKeyHex: String,
        labelHex: String,
        writeProof: String,
    ) {
        val url = "$baseUrl/v1/channels/$channelIdHex/posts/$postCursor/comments/$commentCursor/reactions"
        val response: HttpResponse = anonTokens.sendWithToken(AnonTokenContext.CHANNEL_REACT) { name, value ->
            anonClient.request(url) {
                method = HttpMethod.Post
                header(name, value)
                header(uniffi.lakko_ffi_uniffi.channelWriteProofHeaderName(), writeProof)
                contentType(ContentType.Application.Json)
                setBody(ReactionReq(writerKeyHex, labelHex))
            }
        }
        if (!response.status.isSuccess()) throw decodeError(response, "add comment reaction")
    }

    /** Take a reaction off a comment: a signed delete, no token. */
    public suspend fun deleteCommentReaction(
        channelIdHex: String,
        postCursor: ULong,
        commentCursor: Long,
        writerKeyHex: String,
        labelHex: String,
        writeProof: String,
    ) {
        val response: HttpResponse = anonClient.request(
            "$baseUrl/v1/channels/$channelIdHex/posts/$postCursor/comments/$commentCursor/reactions/$writerKeyHex/$labelHex",
        ) {
            method = HttpMethod.Delete
            header(uniffi.lakko_ffi_uniffi.channelWriteProofHeaderName(), writeProof)
        }
        if (!response.status.isSuccess() && response.status.value != 404) {
            throw decodeError(response, "delete comment reaction")
        }
    }

    @Serializable
    private data class CommentCountsJson(val cursor: Long, val labels: List<LabelCountJson> = emptyList())

    @Serializable
    private data class CommentCountsResp(val comments: List<CommentCountsJson> = emptyList())

    /** Label counts of up to 50 comments under one post, no session. */
    public suspend fun commentReactionCounts(
        channelIdHex: String,
        postCursor: ULong,
        commentCursors: List<Long>,
    ): Map<Long, List<uniffi.lakko_ffi_uniffi.FfiReactionLabelCount>> {
        if (commentCursors.isEmpty()) return emptyMap()
        val response: HttpResponse = anonClient.get(
            "$baseUrl/v1/channels/$channelIdHex/posts/$postCursor/comment-reactions",
        ) {
            parameter("comments", commentCursors.take(50).joinToString(","))
        }
        if (!response.status.isSuccess()) throw decodeError(response, "comment reaction counts")
        return response.body<CommentCountsResp>().comments.associate { c ->
            c.cursor to c.labels.mapNotNull { l ->
                hexOrNull(l.label_hex)?.let { uniffi.lakko_ffi_uniffi.FfiReactionLabelCount(it, l.count.toULong()) }
            }
        }
    }

    /** Section 15 (phase B) — the owner deletes the channel: an owner proof
     *  over the empty body. Afterwards every route answers `410
     *  channel-deleted`. A `410` here is done already. */
    public suspend fun deleteChannel(channelIdHex: String, accountIdentity: Identity) {
        val path = "/v1/channels/$channelIdHex"
        val proof = ownerSigner(accountIdentity, channelIdHex).use {
            it.sign("DELETE", path, "", ByteArray(0))
        }
        val response: HttpResponse = client.request("$baseUrl$path") {
            method = HttpMethod.Delete
            header(channelOwnerProofHeaderName(), proof)
        }
        if (!response.status.isSuccess() && response.status.value != 410) throw decodeError(response, "delete channel")
    }

    @Serializable
    private data class CommentReq(val writer_key_hex: String, val sealed_b64: String)

    @Serializable
    private data class CommentAdded(val cursor: Long)

    /** Add a comment (section 13a): the write proof, then one
     *  `channel-comment` token. Returns the comment cursor. */
    public suspend fun addComment(
        channelIdHex: String,
        postCursor: ULong,
        writerKeyHex: String,
        sealed: ByteArray,
        writeProof: String,
    ): Long {
        val url = "$baseUrl/v1/channels/$channelIdHex/posts/$postCursor/comments"
        val sealedB64 = java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(sealed)
        val response: HttpResponse = anonTokens.sendWithToken(AnonTokenContext.CHANNEL_COMMENT) { name, value ->
            anonClient.request(url) {
                method = HttpMethod.Post
                header(name, value)
                header(uniffi.lakko_ffi_uniffi.channelWriteProofHeaderName(), writeProof)
                contentType(ContentType.Application.Json)
                setBody(CommentReq(writerKeyHex, sealedB64))
            }
        }
        if (!response.status.isSuccess()) throw decodeError(response, "add comment")
        return response.body<CommentAdded>().cursor
    }

    @Serializable
    private data class CommentJson(
        val cursor: Long,
        val writer_key_hex: String,
        val sealed_b64: String,
        val server_ts_ms: Long = 0,
    )

    @Serializable
    private data class CommentsResp(val comments: List<CommentJson> = emptyList(), val next_after: Long = 0)

    /** A page of comments under a post, oldest first, at most 20. */
    public suspend fun listComments(channelIdHex: String, postCursor: ULong, after: Long = 0): CommentPage {
        val response: HttpResponse = anonClient.get("$baseUrl/v1/channels/$channelIdHex/posts/$postCursor/comments") {
            parameter("after", after.toString())
        }
        if (!response.status.isSuccess()) throw decodeError(response, "list comments")
        val body = response.body<CommentsResp>()
        return CommentPage(
            rows = body.comments.mapNotNull { c ->
                val writer = hexOrNull(c.writer_key_hex) ?: return@mapNotNull null
                val sealed = ChannelsComposer.base64UrlDecodeOrNull(c.sealed_b64) ?: return@mapNotNull null
                CommentRow(c.cursor, writer, sealed, c.server_ts_ms)
            },
            nextAfter = body.next_after,
        )
    }

    /** Delete a comment by its author (the comment's own write proof). */
    public suspend fun deleteCommentAsAuthor(
        channelIdHex: String,
        postCursor: ULong,
        commentCursor: Long,
        writeProof: String,
    ) {
        val response: HttpResponse = anonClient.request(
            "$baseUrl/v1/channels/$channelIdHex/posts/$postCursor/comments/$commentCursor",
        ) {
            method = HttpMethod.Delete
            header(uniffi.lakko_ffi_uniffi.channelWriteProofHeaderName(), writeProof)
        }
        if (!response.status.isSuccess() && response.status.value != 404) throw decodeError(response, "delete comment")
    }

    /** Delete a comment as the owner or a publisher (a publisher proof). */
    public suspend fun deleteCommentAsPublisher(
        channelIdHex: String,
        postCursor: ULong,
        commentCursor: Long,
        proofs: ChannelPublisherProofs,
    ) {
        sendAsPublisher(
            proofs,
            HttpMethod.Delete,
            "/v1/channels/$channelIdHex/posts/$postCursor/comments/$commentCursor",
            "",
            ByteArray(0),
            null,
            "delete comment",
        )
    }

    private fun hexOrNull(hex: String): ByteArray? {
        if (hex.length % 2 != 0) return null
        return try {
            ByteArray(hex.length / 2) { i -> hex.substring(i * 2, i * 2 + 2).toInt(16).toByte() }
        } catch (_: NumberFormatException) {
            null
        }
    }

    // -----------------------------------------------------------------
    // ADR-0060 §3 — member cards
    // -----------------------------------------------------------------

    @Serializable
    private data class MemberCardJson(val subscriber_pk_b64: String, val card_b64: String)

    @Serializable
    private data class MemberCardsResp(val members: List<MemberCardJson>, val next_after: String? = null)

    /**
     * `PUT /v1/channels/:id/members/:subscriber_pk_hex` — upload this
     * subscriber's sealed member card. [signature] is
     * `ChannelSubscriberKeypair.memberCardRequestSignature(channelId, card)`,
     * sent in [signatureHeaderName] (`memberCardSigHeaderName()`).
     *
     * Throws [ChannelsApiException]: `unknown-subscriber` (404, the key is not
     * registered on the channel), `member-card-epoch-in-future` /
     * `member-card-stale` (409), `bad-member-signature` (401),
     * `bad-member-card` (400).
     */
    public suspend fun putMemberCard(
        channelIdHex: String,
        subscriberPubkeyHex: String,
        card: ByteArray,
        signatureHeaderName: String,
        signature: String,
    ) {
        val response: HttpResponse = client.put(
            "$baseUrl/v1/channels/$channelIdHex/members/$subscriberPubkeyHex"
        ) {
            header(signatureHeaderName, signature)
            contentType(ContentType.Application.OctetStream)
            setBody(card)
        }
        if (!response.status.isSuccess()) {
            throw decodeError(response, "put member card")
        }
    }

    /** `DELETE /v1/channels/:id/members/:subscriber_pk_hex` — remove this
     *  subscriber's card. [signature] is the signature over the EMPTY card.
     *  Idempotent. */
    public suspend fun deleteMemberCard(
        channelIdHex: String,
        subscriberPubkeyHex: String,
        signatureHeaderName: String,
        signature: String,
    ) {
        val response: HttpResponse = client.delete(
            "$baseUrl/v1/channels/$channelIdHex/members/$subscriberPubkeyHex"
        ) {
            header(signatureHeaderName, signature)
        }
        if (!response.status.isSuccess()) {
            throw decodeError(response, "delete member card")
        }
    }

    /** `GET /v1/channels/:id/members?after=&limit=` — one page of member
     *  cards, ascending by subscriber key. [after] is the previous page's
     *  [MemberCardPage.nextAfter]. An entry whose bytes do not decode is
     *  skipped. */
    public suspend fun listMemberCards(
        channelIdHex: String,
        after: String? = null,
        limit: Int? = null,
    ): MemberCardPage {
        val response: HttpResponse = client.get("$baseUrl/v1/channels/$channelIdHex/members") {
            if (after != null) parameter("after", after)
            if (limit != null) parameter("limit", limit)
        }
        if (!response.status.isSuccess()) {
            throw decodeError(response, "list member cards")
        }
        val body = response.body<MemberCardsResp>()
        return MemberCardPage(
            members = body.members.mapNotNull { m ->
                val pk = ChannelsComposer.base64UrlDecodeOrNull(m.subscriber_pk_b64) ?: return@mapNotNull null
                val card = ChannelsComposer.base64UrlDecodeOrNull(m.card_b64) ?: return@mapNotNull null
                MemberCardEntry(subscriberPk = pk, card = card)
            },
            nextAfter = body.next_after,
        )
    }

    // -----------------------------------------------------------------
    // Error envelope decoder
    // -----------------------------------------------------------------

    @Serializable
    private data class ErrEnvelope(val code: String, val detail: String)

    private suspend fun decodeError(
        response: HttpResponse,
        action: String,
    ): ChannelsApiException {
        return try {
            val env = response.body<ErrEnvelope>()
            ChannelsApiException(response.status.value, env.code, env.detail).also { e ->
                // ADR-0087 section 9 (phase B) — a `410 channel-deleted` from
                // any channels route: the owner deleted that channel.
                if (e.status == 410 && e.code == "channel-deleted") {
                    channelIdInPath(response.call.request.url.encodedPath)?.let { id ->
                        runCatching { onChannelDeleted?.invoke(id) }
                    }
                }
            }
        } catch (_: Throwable) {
            // Some routes return plain text (or no body) on
            // unexpected errors. Capture the text so the caller
            // log line points at the wire content rather than a
            // bare HTTP status.
            val bodyText = runCatching { response.bodyAsText() }.getOrDefault("")
            ChannelsApiException(
                status = response.status.value,
                code = "unknown",
                detail = "$action: ${response.status} $bodyText",
            )
        }
    }

    public companion object {
        /** Told the channel id of every `410 channel-deleted` answer (the
         *  session shell drops a pending publisher request for it). */
        @Volatile
        public var onChannelDeleted: ((channelIdHex: String) -> Unit)? = null

        /** The channel id a `/v1/channels/{id}/...` path names, or `null`. */
        internal fun channelIdInPath(path: String): String? {
            val parts = path.split('/').filter { it.isNotEmpty() }
            // The first `channels/<32 hex>` pair: a prefix such as
            // `/native/channels/v1/channels/...` names the service first.
            for (i in 0 until parts.size - 1) {
                if (parts[i] != "channels") continue
                val id = parts[i + 1].lowercase()
                if (id.length == 32 && id.all { c -> c in '0'..'9' || c in 'a'..'f' }) return id
            }
            return null
        }

        /** The owner signer of [channelIdHex] for the account identity
         *  (ADR-0059 section 1). Only the account that created the channel
         *  derives the key the server registered for it. Deriving it needs
         *  only the identity's seed; the signer outlives the identity call. */
        //
        // ADR-0074 section 3.5: after a rotation of the account keys the
        // owner key is the one the CURRENT account seed derives, while the
        // identity a caller holds (the one its PublisherGroup runs on) stays
        // the conversation identity; [ChannelOwnerSuccession] maps the one
        // to the other here, so every owner request follows the handover.
        public fun ownerSigner(accountIdentity: Identity, channelIdHex: String): ChannelOwnerSigner =
            ChannelOwnerSuccession.withOwnerIdentity(accountIdentity) { ownerSignerExact(it, channelIdHex) }

        /** The owner signer [accountIdentity] ITSELF derives, with no
         *  succession applied: what the owner handover signs with (the old
         *  key) and names (the new key). */
        public fun ownerSignerExact(accountIdentity: Identity, channelIdHex: String): ChannelOwnerSigner =
            channelOwnerSigner(accountIdentity, channelIdBytes(channelIdHex))

        /** The 16 bytes of a 32-hex channel id. */
        public fun channelIdBytes(channelIdHex: String): ByteArray {
            require(channelIdHex.length == 32) { "channel id must be 32 hex chars" }
            return ByteArray(16) { i -> channelIdHex.substring(i * 2, i * 2 + 2).toInt(16).toByte() }
        }

        /** `<LAKKO_API_BASE_URL>/channels` ([ApiEndpoints]), or
         *  `http://10.0.2.2:8086` without an API base URL. */
        public val DEFAULT_BASE_URL: String
            get() = ApiEndpoints.baseUrlFor(ApiService.CHANNELS)

        /** ADR-0030 — the max entries a single
         *  `POST /rotate/stage` chunk may carry. Mirrors the
         *  server's `MAX_STAGE_CHUNK_ENTRIES`; a larger chunk
         *  is refused with 413 `chunk-too-large`, so the
         *  publisher splits its fanout into more chunks. */
        public const val MAX_STAGE_CHUNK_ENTRIES: Int = 5000
    }
}

// ============================================================================
// Wire types
// ============================================================================

/** One subscriber row as a publisher reads it (ADR-0087 section 7). */
public data class SubscriberRow(
    public val seq: Long,
    public val pubkeyHex: String,
    /** The sealed admission ticket of a private channel's row, else `null`. */
    public val sealedTicket: ByteArray?,
)

public data class SubscriberPage(public val rows: List<SubscriberRow>, public val nextAfter: Long)

/** One comment row under a post (ADR-0087 section 13a), still sealed. */
public class CommentRow(
    public val cursor: Long,
    public val writerKey: ByteArray,
    public val sealed: ByteArray,
    public val serverTsMs: Long,
)

public class CommentPage(public val rows: List<CommentRow>, public val nextAfter: Long)

/** One post in the cursor-paginated fetch response. */
public data class PostView(
    public val cursor: ULong,
    public val serverTsMs: ULong,
    /** Opaque ChannelPost bytes, base64url-encoded. */
    public val payloadB64: String,
)

public data class FetchPostsResult(
    public val posts: List<PostView>,
    public val nextAfter: ULong,
)

/** One entry of `GET /v1/channels/:id/members`: the opaque sealed card
 *  under the subscriber key it was uploaded for. */
public class MemberCardEntry(
    /** The 32-byte subscriber key (Ed25519). */
    public val subscriberPk: ByteArray,
    /** The sealed member card. */
    public val card: ByteArray,
)

/** One page of member cards. */
public class MemberCardPage(
    public val members: List<MemberCardEntry>,
    /** The `after` for the next page, `null` when this page was the last. */
    public val nextAfter: String?,
)

/** Server-supplied error envelope — stable across releases. */
public class ChannelsApiException(
    public val status: Int,
    public val code: String,
    public val detail: String,
) : Exception("$code (HTTP $status): $detail")
