// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.transparency

import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.engine.okhttp.OkHttp
import me.lakko.app.transport.AntiCensorshipEngine
import io.ktor.client.plugins.contentnegotiation.ContentNegotiation
import io.ktor.client.request.get
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.bodyAsText
import io.ktor.http.HttpStatusCode
import io.ktor.http.isSuccess
import io.ktor.serialization.kotlinx.json.json
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import me.lakko.app.auth.base64Decode
import me.lakko.app.auth.base64UrlEncodeNoPad

/**
 * Ktor-based client for `lakko-svc-transparency` (KT-log S3e).
 *
 * Mirror of `apps/web/src/transparency/client.ts`. Three anonymous GET
 * endpoints back the inclusion check:
 *
 *   * `GET /transparency/root` → the operator-signed Merkle root
 *     (`{ tree_root_b64, log_size, timestamp_unix, signature_b64 }`).
 *   * `GET /transparency/latest/:account_key_b64url` → the latest entry
 *     for an account-key (`{ idx, version, identity_pubkey_b64, is_reset }`)
 *     or 404 when the account is not yet logged.
 *   * `GET /transparency/proof/:idx` → an inclusion proof
 *     (`{ idx, leaf_b64, audit_path: [b64...], log_size }`).
 *
 * The client decodes the base64 fields to raw bytes so the verify layer
 * hands them straight into the UniFFI `verifyLogInclusion`. It is
 * transport-only: it never verifies signatures (that is
 * [TransparencyVerify]'s job, against the build-time pinned operator key)
 * and never consults the `/transparency/operator-key` endpoint for trust
 * (the operator key is pinned out-of-band via [TransparencyConfig]).
 *
 * Uses the same Ktor + OkHttp stack [me.lakko.app.discovery.DiscoveryClient]
 * does; tests inject a `MockEngine` for deterministic responses without a
 * network.
 *
 * [forOptionalConfig] returns `null` when the transparency integration is
 * OFF (URL or operator-key pin unset) so the caller can skip the whole
 * check; see [TransparencyConfig] for the gradle-property contract.
 */
public class TransparencyClient(
    public val baseUrl: String,
    /** Ktor engine. Production: `OkHttp` (default below). Tests:
     *  `MockEngine` for deterministic responses without a network. */
    engine: HttpClientEngine? = null,
) : AutoCloseable {

    private val json = Json {
        ignoreUnknownKeys = true
        explicitNulls = false
    }

    private val client: HttpClient = if (engine != null) {
        HttpClient(engine) {
            install(ContentNegotiation) { json(json) }
        }
    } else {
        HttpClient(AntiCensorshipEngine(OkHttp.create {})) {
            install(ContentNegotiation) { json(json) }
        }
    }

    override fun close() {
        client.close()
    }

    /** `GET /transparency/root`. */
    public suspend fun fetchRoot(): SignedRoot {
        val wire: RootResWire = getJson("/transparency/root")
        return SignedRoot(
            treeRoot = base64Decode(wire.tree_root_b64),
            logSize = wire.log_size,
            timestampUnix = wire.timestamp_unix,
            signature = base64Decode(wire.signature_b64),
            // ADR-0020: optional witness co-signature (absent when the
            // operator runs without a witness). The witness gate fails
            // closed when a witness key is pinned and this is null.
            witnessCosig = wire.witness_cosig_b64
                ?.takeIf { it.isNotEmpty() }
                ?.let { base64Decode(it) },
        )
    }

    /**
     * `GET /transparency/latest/:account_key`. The 32-byte account-key is
     * encoded URL-safe-no-pad for the path segment (the server accepts
     * either alphabet). Returns `null` on 404 — the account is not yet in
     * the log.
     */
    public suspend fun fetchLatest(usernameHash: ByteArray): LatestEntry? {
        val slug = base64UrlEncodeNoPad(usernameHash)
        val wire: LatestResWire = getJsonOr404("/transparency/latest/$slug") ?: return null
        return LatestEntry(
            idx = wire.idx,
            version = wire.version,
            identityPubkey = base64Decode(wire.identity_pubkey_b64),
            isReset = wire.is_reset,
            leafAuth = wire.leaf_auth_b64?.let { base64Decode(it) },
            genesisAttest = wire.genesis_attest_b64?.let { base64Decode(it) },
        )
    }

    /** `GET /transparency/proof/:idx`. */
    public suspend fun fetchProof(idx: ULong): InclusionProof {
        val wire: ProofResWire = getJson("/transparency/proof/$idx")
        // Flatten the audit path: each entry is a base64 32-byte sibling
        // hash; concatenate them so the UniFFI verifier reads a single
        // `ByteArray` whose length is a multiple of 32.
        val hashes = wire.audit_path.map { base64Decode(it) }
        val flat = ByteArray(hashes.sumOf { it.size })
        var off = 0
        for (h in hashes) {
            h.copyInto(flat, off)
            off += h.size
        }
        return InclusionProof(
            idx = wire.idx,
            leaf = base64Decode(wire.leaf_b64),
            auditPath = flat,
            logSize = wire.log_size,
        )
    }

    // -----------------------------------------------------------------
    // Internals
    // -----------------------------------------------------------------

    private suspend inline fun <reified Res : Any> getJson(endpoint: String): Res =
        getJsonOr404<Res>(endpoint)
            ?: throw TransparencyHttpError(404, endpoint, "unexpected 404")

    /**
     * GET + JSON, folding the well-known 404 into `null`. Any other
     * non-2xx throws [TransparencyHttpError]; a transport failure throws
     * [TransparencyNetworkError].
     */
    private suspend inline fun <reified Res : Any> getJsonOr404(endpoint: String): Res? {
        val url = "$baseUrl$endpoint"
        val response: HttpResponse = try {
            client.get(url)
        } catch (cause: Throwable) {
            throw TransparencyNetworkError(endpoint, cause)
        }
        if (response.status == HttpStatusCode.NotFound) return null
        if (!response.status.isSuccess()) {
            throw TransparencyHttpError(
                status = response.status.value,
                endpoint = endpoint,
                detail = response.safeReadBody(),
            )
        }
        return try {
            response.body()
        } catch (cause: Throwable) {
            throw TransparencyHttpError(
                status = response.status.value,
                endpoint = endpoint,
                detail = "body is not valid JSON: $cause",
            )
        }
    }

    public companion object {
        /**
         * Build a client from the build-time config, or return `null`
         * when the transparency integration is OFF (URL or operator-key
         * pin unset). The caller skips the whole log check on `null`.
         * Mirror of the web `TransparencyClient.forOptionalConfig()`.
         */
        public fun forOptionalConfig(engine: HttpClientEngine? = null): TransparencyClient? {
            val base = TransparencyConfig.baseUrl() ?: return null
            // The operator key must also be present, or there is nothing
            // to anchor verification against — treat as OFF.
            if (TransparencyConfig.pinnedOperatorKey() == null) return null
            return TransparencyClient(base, engine)
        }
    }
}

private suspend fun HttpResponse.safeReadBody(): String =
    try {
        val text = bodyAsText()
        if (text.length > 256) "${text.take(256)}…" else text
    } catch (cause: Throwable) {
        "<unreadable body: ${cause::class.simpleName}>"
    }

// =============================================================================
// Decoded responses — bytes, not base64
// =============================================================================

/** Decoded `GET /transparency/root` response. */
public data class SignedRoot(
    /** 32-byte Merkle root over the first [logSize] entries. */
    val treeRoot: ByteArray,
    /** Number of entries the root commits to. */
    val logSize: ULong,
    /** Operator-chosen timestamp (seconds since UNIX epoch). */
    val timestampUnix: ULong,
    /** 64-byte Ed25519 signature over the canonical root pre-image. */
    val signature: ByteArray,
    /** ADR-0020: the witness co-signature wire blob over `(treeRoot,
     *  logSize)`, or `null` when the server serves no witness
     *  co-signature. A client that pins a witness key requires a valid one
     *  (the split-view defence). */
    val witnessCosig: ByteArray? = null,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is SignedRoot) return false
        if (!treeRoot.contentEquals(other.treeRoot)) return false
        if (logSize != other.logSize) return false
        if (timestampUnix != other.timestampUnix) return false
        if (!signature.contentEquals(other.signature)) return false
        val a = witnessCosig
        val b = other.witnessCosig
        if (a == null) return b == null
        return b != null && a.contentEquals(b)
    }

    override fun hashCode(): Int {
        var result = treeRoot.contentHashCode()
        result = 31 * result + logSize.hashCode()
        result = 31 * result + timestampUnix.hashCode()
        result = 31 * result + signature.contentHashCode()
        result = 31 * result + (witnessCosig?.contentHashCode() ?: 0)
        return result
    }
}

/** Decoded `GET /transparency/latest/:account_key` response. */
public data class LatestEntry(
    /** 0-based leaf index of the latest entry (== the `/proof` idx). */
    val idx: ULong,
    /** Server-assigned monotonic per-account-key version (>= 1). */
    val version: ULong,
    /** The account's current identity public key (raw bytes). */
    val identityPubkey: ByteArray,
    /** ADR-0017 fresh-start marker — `true` if the latest entry was a
     *  no-supersession-proof owner reset. */
    val isReset: Boolean,
    /** G-30 #2 (ADR-0019): the entry's opaque leaf authorisation (a 64-byte
     *  genesis self-sig or a 145-byte supersession proof), or `null` for a
     *  pre-Sprint-2 / unauthenticated row. A client verifies a genesis
     *  self-sig against the served key on a first observation. */
    val leafAuth: ByteArray? = null,
    /** ADR-0036 (G-30 finding 2): the entry's opaque auth-attested genesis
     *  attestation (a 153-byte `GenesisAttestation`), or `null` for a
     *  genesis leaf minted before ADR-0036 / a rotation entry. When present,
     *  a client verifies it against the pinned auth attest verifying key on a
     *  first observation — this PREVENTS first-entry squatting where the
     *  self-sig only detects it. */
    val genesisAttest: ByteArray? = null,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is LatestEntry) return false
        if (idx != other.idx) return false
        if (version != other.version) return false
        if (!identityPubkey.contentEquals(other.identityPubkey)) return false
        if (isReset != other.isReset) return false
        val a = leafAuth
        val b = other.leafAuth
        if (a == null) {
            if (b != null) return false
        } else if (b == null || !a.contentEquals(b)) {
            return false
        }
        val ga = genesisAttest
        val gb = other.genesisAttest
        if (ga == null) return gb == null
        return gb != null && ga.contentEquals(gb)
    }

    override fun hashCode(): Int {
        var result = idx.hashCode()
        result = 31 * result + version.hashCode()
        result = 31 * result + identityPubkey.contentHashCode()
        result = 31 * result + isReset.hashCode()
        result = 31 * result + (leafAuth?.contentHashCode() ?: 0)
        result = 31 * result + (genesisAttest?.contentHashCode() ?: 0)
        return result
    }
}

/** Decoded `GET /transparency/proof/:idx` response. The [auditPath] is
 *  FLATTENED (each 32-byte sibling hash concatenated leaf→root) so it
 *  feeds straight into `verifyLogInclusion`. */
public data class InclusionProof(
    /** 0-based leaf index of the proven entry. */
    val idx: ULong,
    /** The entry's canonical leaf bytes (informational; the verify path
     *  binds the decomposed `(account_key, pubkey, version)` instead). */
    val leaf: ByteArray,
    /** Flattened audit path: sibling node-hashes concatenated leaf→root,
     *  length always a multiple of 32. */
    val auditPath: ByteArray,
    /** Tree size the proof was generated against. */
    val logSize: ULong,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is InclusionProof) return false
        return idx == other.idx &&
            leaf.contentEquals(other.leaf) &&
            auditPath.contentEquals(other.auditPath) &&
            logSize == other.logSize
    }

    override fun hashCode(): Int {
        var result = idx.hashCode()
        result = 31 * result + leaf.contentHashCode()
        result = 31 * result + auditPath.contentHashCode()
        result = 31 * result + logSize.hashCode()
        return result
    }
}

// =============================================================================
// Errors
// =============================================================================

/** Network-level failure (DNS, TLS, no route, …). Distinct from a parse
 *  failure so [TransparencyVerify] can map both to `Unreachable`. */
public class TransparencyNetworkError(
    public val endpoint: String,
    cause: Throwable,
) : RuntimeException("transparency $endpoint network error: $cause", cause)

/** Server returned a non-2xx (and not the well-known 404) status, or a
 *  body that did not decode to the expected shape. */
public class TransparencyHttpError(
    public val status: Int,
    public val endpoint: String,
    public val detail: String,
) : RuntimeException("transparency $endpoint → $status: $detail")

// =============================================================================
// Wire shapes (kotlinx-serialization) — match the Rust handlers
// =============================================================================

/** Wire shape of `GET /transparency/root` (Rust `RootRes`). */
@Serializable
private data class RootResWire(
    val tree_root_b64: String,
    val log_size: ULong,
    val timestamp_unix: ULong,
    val signature_b64: String,
    // ADR-0020: base64 witness co-signature, absent when unconfigured.
    val witness_cosig_b64: String? = null,
)

/** Wire shape of `GET /transparency/latest/:key` (Rust `LatestRes`). */
@Serializable
private data class LatestResWire(
    val idx: ULong,
    val version: ULong,
    val identity_pubkey_b64: String,
    val is_reset: Boolean,
    // G-30 #2 (ADR-0019): base64 of the entry's leaf authorisation, or
    // absent for a pre-Sprint-2 / unauthenticated row.
    val leaf_auth_b64: String? = null,
    // ADR-0036 (G-30 finding 2): standard base64 of the entry's
    // auth-attested genesis attestation, or absent for a genesis leaf
    // minted before ADR-0036 / a rotation entry.
    val genesis_attest_b64: String? = null,
)

/** Wire shape of `GET /transparency/proof/:idx` (Rust `ProofRes`). */
@Serializable
private data class ProofResWire(
    val idx: ULong,
    val leaf_b64: String,
    val audit_path: List<String>,
    val log_size: ULong,
)
