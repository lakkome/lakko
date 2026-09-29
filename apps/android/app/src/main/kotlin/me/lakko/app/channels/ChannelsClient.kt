// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.engine.okhttp.OkHttp
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.config.ApiService
import me.lakko.app.transport.AntiCensorshipEngine
import io.ktor.client.plugins.contentnegotiation.ContentNegotiation
import io.ktor.client.request.delete
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.parameter
import io.ktor.client.request.post
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
 * `http://10.0.2.2:8090` — Android-emulator loopback
 * alias + the services/channels dev bind. Override on
 * physical devices (LAN IP) and in production
 * (Cloudflare tunnel URL).
 */
public class ChannelsClient(
    public val baseUrl: String = DEFAULT_BASE_URL,
    engine: HttpClientEngine? = null,
) : AutoCloseable {

    private val jsonCodec = Json {
        ignoreUnknownKeys = true
        explicitNulls = false
    }

    private val client: HttpClient = if (engine != null) {
        HttpClient(engine) { install(ContentNegotiation) { json(jsonCodec) } }
    } else {
        HttpClient(AntiCensorshipEngine(OkHttp.create {})) { install(ContentNegotiation) { json(jsonCodec) } }
    }

    override fun close() {
        client.close()
    }

    // -----------------------------------------------------------------
    // Channels
    // -----------------------------------------------------------------

    @Serializable
    private data class CreateReq(val channel_id: String)

    @Serializable
    private data class CreateResp(val channel_id: String)

    /** `POST /v1/channels` — register a fresh channel. */
    public suspend fun createChannel(channelIdHex: String): String {
        val response: HttpResponse = client.post("$baseUrl/v1/channels") {
            contentType(ContentType.Application.Json)
            setBody(CreateReq(channelIdHex))
        }
        if (!response.status.isSuccess()) {
            throw decodeError(response, "create channel")
        }
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

    /** `POST /v1/channels/:id/posts` — publish opaque ChannelPost bytes. */
    public suspend fun publishPost(
        channelIdHex: String,
        payload: ByteArray,
    ): PostView {
        val response: HttpResponse = client.post(
            "$baseUrl/v1/channels/$channelIdHex/posts"
        ) {
            contentType(ContentType.Application.OctetStream)
            setBody(payload)
        }
        if (!response.status.isSuccess()) {
            throw decodeError(response, "publish post")
        }
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
     *  subscriber's X25519 public key. */
    public suspend fun subscribe(
        channelIdHex: String,
        subscriberPubkeyHex: String,
    ): String {
        val response: HttpResponse = client.post(
            "$baseUrl/v1/channels/$channelIdHex/subscribers"
        ) {
            contentType(ContentType.Application.Json)
            setBody(SubscribeReq(subscriberPubkeyHex))
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
     *  to fan out HPKE-sealed seed envelopes. */
    public suspend fun listSubscribers(channelIdHex: String): List<String> {
        val response: HttpResponse = client.get(
            "$baseUrl/v1/channels/$channelIdHex/subscribers"
        )
        if (!response.status.isSuccess()) {
            throw decodeError(response, "list subscribers")
        }
        return response.body<SubscribersResp>().subscribers
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
     *  backward-compatible shape). */
    public suspend fun deliverEnvelope(
        channelIdHex: String,
        subscriberPubkeyHex: String,
        envelope: ByteArray,
        epoch: Int? = null,
    ) {
        val response: HttpResponse = client.post(
            "$baseUrl/v1/channels/$channelIdHex/subscribers/$subscriberPubkeyHex/envelope"
        ) {
            contentType(ContentType.Application.OctetStream)
            if (epoch != null) parameter("epoch", epoch.toString())
            setBody(envelope)
        }
        if (!response.status.isSuccess()) {
            throw decodeError(response, "deliver envelope")
        }
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
    ): UInt {
        val response: HttpResponse = client.post(
            "$baseUrl/v1/channels/$channelIdHex/rotate"
        ) {
            contentType(ContentType.Application.Json)
            setBody(RotateRequest(envelopes))
        }
        if (!response.status.isSuccess()) {
            throw decodeError(response, "rotate channel")
        }
        val view = response.body<EpochView>()
        return view.epoch
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
    ) {
        val response: HttpResponse = client.post(
            "$baseUrl/v1/channels/$channelIdHex/rotate/stage"
        ) {
            contentType(ContentType.Application.Json)
            setBody(StageRotateRequest(targetEpoch, envelopes))
        }
        if (!response.status.isSuccess()) {
            throw decodeError(response, "stage rotation")
        }
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
    ): UInt {
        val response: HttpResponse = client.post(
            "$baseUrl/v1/channels/$channelIdHex/rotate/commit"
        ) {
            contentType(ContentType.Application.Json)
            setBody(CommitRotateRequest(targetEpoch))
        }
        if (!response.status.isSuccess()) {
            throw decodeError(response, "commit rotation")
        }
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
            ChannelsApiException(response.status.value, env.code, env.detail)
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
        /** `<LAKKO_API_BASE_URL>/channels` ([ApiEndpoints]), or
         *  `http://10.0.2.2:8090` without an API base URL. */
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

/** Server-supplied error envelope — stable across releases. */
public class ChannelsApiException(
    public val status: Int,
    public val code: String,
    public val detail: String,
) : Exception("$code (HTTP $status): $detail")
