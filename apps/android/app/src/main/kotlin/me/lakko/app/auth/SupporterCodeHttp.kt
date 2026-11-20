// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
import io.ktor.client.HttpClient
import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.contentType
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import me.lakko.app.anon.AnonymousHttp
import me.lakko.app.anon.KtorAnonTokenIssuer
import me.lakko.app.anon.errorCodeOf
import me.lakko.app.anon.longFieldOf
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.config.ApiService
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.SupporterCodeRequest
import uniffi.lakko_ffi_uniffi.betaPlaceContextName
import uniffi.lakko_ffi_uniffi.betaPlaceWindowSecs
import uniffi.lakko_ffi_uniffi.supporterCodeEpochAt
import uniffi.lakko_ffi_uniffi.supporterCodeNormalize
import kotlin.coroutines.cancellation.CancellationException

/** The `beta-place` context's wire name (a literal so host tests need no
 *  native library; [UniffiSupporterCodeCore] reads the core's). */
public const val BETA_PLACE_CONTEXT: String = "beta-place"

@Serializable
@Suppress("ConstructorParameterNaming")
private data class RedeemWireResponse(
    val context: String,
    val epoch: Long,
    val evaluation: String,
    val proof: String,
)

private val lenientJson = Json { ignoreUnknownKeys = true }

/**
 * `POST /auth/supporter-code/redeem` and the `beta-place` key route over the
 * anonymous engine ([AnonymousHttp]: its own connection pool, no cookies, no
 * `Authorization`, through the anti-censorship tunnel when armed).
 */
public class KtorSupporterCodeApi(
    private val baseUrl: String = ApiEndpoints.baseUrlFor(ApiService.AUTH),
    engine: HttpClientEngine? = null,
    private val keys: KtorAnonTokenIssuer = KtorAnonTokenIssuer(baseUrl = baseUrl, engine = engine, anonEngine = engine),
    private val contextName: String = BETA_PLACE_CONTEXT,
) : SupporterCodeApi {
    private val client: HttpClient = HttpClient(engine ?: AnonymousHttp.engine)

    override suspend fun redeem(bodyJson: String): SupporterRedeemWire {
        val response: HttpResponse = try {
            client.post("$baseUrl$REDEEM_PATH") {
                contentType(ContentType.Application.Json)
                setBody(bodyJson)
            }
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            return SupporterRedeemWire.Failed
        }
        val text = runCatching { response.bodyAsText() }.getOrNull()
        return supporterRedeemWireOf(response.status.value, text, contextName)
    }

    override suspend fun publicKey(epoch: Long): String? = keys.publicKeyForWire(contextName, epoch)

    public companion object {
        public const val REDEEM_PATH: String = "/auth/supporter-code/redeem"
    }
}

/** The redeem answer read from its status and body (ADR-0097 "As built"). */
public fun supporterRedeemWireOf(status: Int, body: String?, contextName: String = BETA_PLACE_CONTEXT): SupporterRedeemWire {
    val code = errorCodeOf(body)
    return when {
        status in 200..299 -> try {
            val parsed = lenientJson.decodeFromString(RedeemWireResponse.serializer(), body ?: "")
            if (parsed.context != contextName) {
                SupporterRedeemWire.Failed
            } else {
                SupporterRedeemWire.Evaluated(parsed.epoch, parsed.evaluation, parsed.proof)
            }
        } catch (_: Throwable) {
            SupporterRedeemWire.Failed
        }
        status == 400 && code == "epoch-not-issuable" ->
            longFieldOf(body, "current_epoch")?.let { SupporterRedeemWire.EpochNotIssuable(it) }
                ?: SupporterRedeemWire.Failed
        code in UNKNOWN_CODES -> SupporterRedeemWire.Unknown
        status == 429 && code == "supporter-code-limited" -> SupporterRedeemWire.Limited
        status == 429 -> SupporterRedeemWire.Limited
        else -> SupporterRedeemWire.Failed
    }
}

private val UNKNOWN_CODES: Set<String> = setOf(
    "supporter-code-unknown",
    "supporter-code-expired",
    "supporter-code-mismatch",
    "supporter-code-malformed",
)

/** [SupporterCodeCore] over the UniFFI bindings. */
public object UniffiSupporterCodeCore : SupporterCodeCore {
    override fun normalize(text: String): String? = try {
        supporterCodeNormalize(text)
    } catch (_: FfiException) {
        null
    }

    override fun epochAt(unixSecs: Long): Long = supporterCodeEpochAt(unixSecs.toULong()).toLong()

    override val windowSecs: Long by lazy { betaPlaceWindowSecs().toLong() }

    /** The core's name for the context. */
    public val contextName: String by lazy { betaPlaceContextName() }

    override fun begin(code: String, epoch: Long): SupporterCodeRequestHandle {
        val request = SupporterCodeRequest(code, epoch.toULong())
        return object : SupporterCodeRequestHandle {
            override val epoch: Long = request.epoch().toLong()

            override fun redeemBodyJson(): String = request.redeemBodyJson()

            override fun finalize(evaluationB64url: String, proofB64url: String, publicKeyB64url: String): String =
                request.finalize(evaluationB64url, proofB64url, publicKeyB64url)

            override fun close() {
                request.close()
            }
        }
    }
}

/** The supporter places of this device (pre-account rows in their own
 *  encrypted preferences file; the app allows no backup). */
public object SupporterPlacesOnDevice {
    private const val PREFS_NAME: String = "lakko_supporter_place"

    @Volatile
    private var instance: SupporterPlaces? = null

    public fun get(context: Context): SupporterPlaces =
        instance ?: synchronized(this) {
            instance ?: build(context.applicationContext).also { instance = it }
        }

    private fun build(app: Context): SupporterPlaces {
        val core = UniffiSupporterCodeCore
        val store = SupporterPlaceStore(
            storage = EncryptedMarkerStorage(app, PREFS_NAME),
            windowSecs = { core.windowSecs },
            epochAt = core::epochAt,
        )
        return StoredSupporterPlaces(
            store = store,
            redeemer = SupporterCodeRedeemer(core, KtorSupporterCodeApi(contextName = core.contextName)),
        )
    }
}
