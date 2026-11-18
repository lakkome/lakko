// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.donate

import io.ktor.client.HttpClient
import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.request.get
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.bodyAsBytes
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.contentType
import io.ktor.http.encodeURLPathPart
import io.ktor.http.isSuccess
import java.io.IOException
import java.security.MessageDigest
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import me.lakko.app.anon.AnonymousHttp
import me.lakko.app.discovery.DiscoveryClient
import uniffi.lakko_ffi_uniffi.DonationKind
import uniffi.lakko_ffi_uniffi.verifyDonationMethod
import uniffi.lakko_ffi_uniffi.verifyDonationPhone
import kotlin.coroutines.cancellation.CancellationException

/*
 * The ways to donate (docs/ux/donations.md sections 3 and 4.1): the three
 * anonymous discovery routes the Support Lakko screen reads. Mirror of the web
 * `src/donate/client.ts`.
 *
 * Every method is a statement signed offline by the donations key; this
 * client verifies each one against the key pinned into the build
 * (`BuildConfig.LAKKO_DONATIONS_KEY`) and keeps only what verifies. A build
 * with no pinned key shows no method and asks the server nothing. Nothing
 * here logs a method, a link, an address or a number.
 */

/** What kind of card a method is drawn as. */
public enum class DonateKind { SBP, REVOLUT, BMC, CRYPTO }

/** The coin, network and address of a crypto method. */
public data class DonateCrypto(val coin: String, val network: String, val address: String)

/** One verified way to donate. Every field comes from the signed statement. */
public class DonateMethod(
    public val id: String,
    public val kind: DonateKind,
    /** The payment link, `https://` only. */
    public val link: String?,
    /** SHA-256 of the QR image the server serves (32 bytes), or `null`. */
    public val qrSha256: ByteArray?,
    public val hasPhone: Boolean,
    public val crypto: DonateCrypto?,
)

/** What `verifyDonationMethod` returns, without the generated type, so the
 *  screen logic runs on a host JVM with no native library. */
public class VerifiedDonationStatement(
    public val id: String,
    public val kind: DonateKind,
    public val link: String?,
    public val coin: String?,
    public val network: String?,
    public val address: String?,
    public val qrSha256: ByteArray?,
    public val hasPhone: Boolean,
)

/** The two signature checks. Each throws on any refusal. */
public interface DonationVerifier {
    public fun verifyMethod(statement: ByteArray, key: ByteArray): VerifiedDonationStatement
    public fun verifyPhone(statement: ByteArray, key: ByteArray, id: String): String
}

/** The real checks: the UniFFI core. */
public object FfiDonationVerifier : DonationVerifier {
    override fun verifyMethod(statement: ByteArray, key: ByteArray): VerifiedDonationStatement {
        val m = verifyDonationMethod(statement, key)
        return VerifiedDonationStatement(
            id = m.id,
            kind = when (m.kind) {
                DonationKind.SBP -> DonateKind.SBP
                DonationKind.REVOLUT -> DonateKind.REVOLUT
                DonationKind.BMC -> DonateKind.BMC
                DonationKind.CRYPTO -> DonateKind.CRYPTO
            },
            link = m.link,
            coin = m.coin,
            network = m.network,
            address = m.address,
            qrSha256 = m.qrSha256,
            hasPhone = m.hasPhone,
        )
    }

    override fun verifyPhone(statement: ByteArray, key: ByteArray, id: String): String =
        verifyDonationPhone(statement, key, id)
}

/** What the screen asks for; [DonationsClient] in production, a fake in tests. */
public interface DonationsSource {
    /** The verified methods in the server's order; empty with no pinned key.
     *  Throws on a network or server failure. */
    public suspend fun methods(): List<DonateMethod>

    /** The QR image bytes, only when they hash to [expectedSha256]. */
    public suspend fun qr(id: String, expectedSha256: ByteArray): ByteArray?

    /** The verified phone number. Throws when it is not given or does not verify. */
    public suspend fun phone(id: String): String
}

/** A network or server failure of the list route (the Error state). */
public class DonationsUnavailable(message: String) : IOException(message)

private const val KEY_LEN = 32
private const val SHA256_LEN = 32

/** Largest QR image accepted (the signing tool and discovery refuse larger). */
internal const val MAX_QR_BYTES: Int = 128 * 1024

/** The 32-byte key from its base64url (no padding) form, or `null` when
 *  blank, malformed or not 32 bytes. There is no fallback. */
public fun parseDonationsKey(raw: String?): ByteArray? {
    val text = raw?.trim().orEmpty()
    if (text.isEmpty()) return null
    return try {
        java.util.Base64.getUrlDecoder().decode(text).takeIf { it.size == KEY_LEN }
    } catch (_: IllegalArgumentException) {
        null
    }
}

/** The key pinned into this build. */
public fun pinnedDonationsKey(): ByteArray? = parseDonationsKey(me.lakko.app.BuildConfig.LAKKO_DONATIONS_KEY)

private fun nonBlank(v: String?): String? = v?.takeIf { it.isNotBlank() }

private fun decodeStatement(element: Any?): ByteArray? {
    val p = element as? JsonPrimitive ?: return null
    if (!p.isString || p.content.isEmpty()) return null
    return try {
        java.util.Base64.getDecoder().decode(p.content)
    } catch (_: IllegalArgumentException) {
        null
    }
}

/** Read one verified statement into a method, or `null` when it is not one
 *  this client draws. */
private fun toMethod(v: VerifiedDonationStatement): DonateMethod? {
    val link = nonBlank(v.link)?.takeIf { it.startsWith("https://") }
    val qr = v.qrSha256?.takeIf { it.size == SHA256_LEN }?.copyOf()
    if (v.kind == DonateKind.CRYPTO) {
        val coin = nonBlank(v.coin) ?: return null
        val network = nonBlank(v.network) ?: return null
        val address = nonBlank(v.address) ?: return null
        return DonateMethod(v.id, v.kind, null, null, false, DonateCrypto(coin, network, address))
    }
    if (link == null && qr == null && !v.hasPhone) return null
    return DonateMethod(v.id, v.kind, link, qr, v.hasPhone, null)
}

/**
 * Verify the list route's answer against [key]. An entry is dropped when its
 * statement does not decode or verify, when the signed id differs from the
 * entry's id, when it repeats an id already kept, or when it is not a method
 * this client can draw. Order is the server's.
 */
public fun verifyDonateMethods(body: String, key: ByteArray, verifier: DonationVerifier): List<DonateMethod> {
    val root = try {
        Json.parseToJsonElement(body) as? JsonObject
    } catch (_: Exception) {
        null
    } ?: return emptyList()
    val list = root["methods"] as? JsonArray ?: return emptyList()
    val out = ArrayList<DonateMethod>()
    val seen = HashSet<String>()
    for (raw in list) {
        val entry = raw as? JsonObject ?: continue
        val idPrim = entry["id"] as? JsonPrimitive ?: continue
        if (!idPrim.isString) continue
        val entryId = idPrim.content
        val statement = decodeStatement(entry["statement_b64"]) ?: continue
        val verified = try {
            verifier.verifyMethod(statement, key)
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Exception) {
            continue
        }
        if (verified.id != entryId || verified.id in seen) continue
        val method = toMethod(verified) ?: continue
        seen += method.id
        out += method
    }
    return out
}

/** Whether [bytes] hash to [expectedSha256] (all 32 bytes, constant time). */
internal fun qrMatches(bytes: ByteArray, expectedSha256: ByteArray): Boolean {
    if (expectedSha256.size != SHA256_LEN) return false
    if (bytes.isEmpty() || bytes.size > MAX_QR_BYTES) return false
    val digest = MessageDigest.getInstance("SHA-256").digest(bytes)
    return MessageDigest.isEqual(digest, expectedSha256)
}

/**
 * The three anonymous routes over the anonymous engine (its own connection
 * pool, never `Authorization`, the anti-censorship tunnel when armed).
 */
public class DonationsClient(
    private val baseUrl: String = DiscoveryClient.DEFAULT_BASE_URL,
    engine: HttpClientEngine? = null,
    private val key: ByteArray? = pinnedDonationsKey(),
    private val verifier: DonationVerifier = FfiDonationVerifier,
) : DonationsSource, AutoCloseable {

    private val client: HttpClient = HttpClient(engine ?: AnonymousHttp.engine)

    private fun idPath(id: String): String = id.encodeURLPathPart()

    override suspend fun methods(): List<DonateMethod> {
        val k = key ?: return emptyList()
        val body = try {
            val res = client.get("$baseUrl/discovery/donate")
            if (!res.status.isSuccess()) throw DonationsUnavailable("donate list ${res.status.value}")
            res.bodyAsText()
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (e: DonationsUnavailable) {
            throw e
        } catch (e: Exception) {
            throw DonationsUnavailable("donate list ${e.javaClass.simpleName}")
        }
        return verifyDonateMethods(body, k, verifier)
    }

    override suspend fun qr(id: String, expectedSha256: ByteArray): ByteArray? {
        if (expectedSha256.size != SHA256_LEN) return null
        return try {
            val res = client.get("$baseUrl/discovery/donate/${idPath(id)}/qr")
            if (!res.status.isSuccess()) return null
            val bytes = res.bodyAsBytes()
            if (qrMatches(bytes, expectedSha256)) bytes else null
        } catch (cancellation: CancellationException) {
            throw cancellation
        } catch (_: Exception) {
            null
        }
    }

    override suspend fun phone(id: String): String {
        val k = key ?: throw IOException("donate phone: no pinned key")
        val res = client.post("$baseUrl/discovery/donate/${idPath(id)}/phone") {
            contentType(ContentType.Application.Json)
            setBody("""{"action":"reveal"}""")
        }
        if (!res.status.isSuccess()) throw IOException("donate phone ${res.status.value}")
        val root = Json.parseToJsonElement(res.bodyAsText()) as? JsonObject
            ?: throw IOException("donate phone: no body")
        val statement = decodeStatement(root["statement_b64"]) ?: throw IOException("donate phone: no statement")
        return nonBlank(verifier.verifyPhone(statement, k, id)) ?: throw IOException("donate phone empty")
    }

    override fun close() {
        client.close()
    }
}

/** The masked form shown before the number is revealed. */
public const val MASKED_PHONE: String = "+7 9•• ••• •• ••"
