// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.engine.okhttp.OkHttp
import io.ktor.client.plugins.contentnegotiation.ContentNegotiation
import io.ktor.client.request.get
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
        HttpClient(OkHttp) { install(ContentNegotiation) { json(jsonCodec) } }
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
        public const val DEFAULT_BASE_URL: String = "http://10.0.2.2:8090"
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
