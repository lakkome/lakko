// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.engine.okhttp.OkHttp
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.config.ApiService
import me.lakko.app.transport.AntiCensorshipEngine
import io.ktor.client.plugins.contentnegotiation.ContentNegotiation
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
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import uniffi.lakko_ffi_uniffi.OpaqueLogin
import uniffi.lakko_ffi_uniffi.OpaqueRegister
import uniffi.lakko_ffi_uniffi.RecoveryMnemonic

/**
 * Ktor-based client for `lakko-svc-auth`.
 *
 * Mirror of `apps/web/src/auth/client.ts`, translated to Kotlin
 * idioms: suspending functions instead of `Promise`s,
 * `@Serializable` data classes instead of TypeScript interfaces,
 * Ktor's [HttpClient] with OkHttp engine instead of `fetch`. The
 * wire format is byte-identical — both clients hit the same
 * `lakko-svc-auth` HTTP endpoints.
 *
 * Two layers of API:
 *
 *   1. **Low-level methods** mirror the seven HTTP endpoints 1:1.
 *      Take a typed request, return a typed response, do nothing
 *      else. Useful for tests, for the recovery flow, and for the
 *      Compose-side state machines that 1.15d4–6 build on top.
 *
 *   2. **High-level [signup] / [login] / [recover]** orchestrate
 *      the full multi-round-trip flow:
 *      `/pow/challenge` → `/signup/start` → `/signup/finish`
 *      (and the equivalent, with no proof of work, for login and
 *      recover). They drive the UniFFI OPAQUE handles
 *      ([OpaqueRegister] / [OpaqueLogin]), the BIP39
 *      [RecoveryMnemonic], and the [solvePow] helper. Since
 *      ADR-0054 S4 signup start is the ONLY request that pays a
 *      proof of work.
 *
 * ## Lifecycle
 *
 * One `AuthClient` per app process is the recommended pattern —
 * Ktor's [HttpClient] is heavy (it owns the OkHttp connection pool
 * + dispatcher thread); construct it once at app start and reuse.
 * [close] disposes the client when the app shuts down (rarely
 * useful in practice — the JVM exit tears the sockets down anyway).
 *
 * ## Default base URL
 *
 * `http://10.0.2.2:8081` — the canonical "host machine from inside
 * the Android emulator" address. On a physical device pointed at a
 * dev workstation, override via the constructor with the
 * workstation's LAN IP. Production switches to
 * `https://api-dev.lakko.me` (1.15d's deployment notes) via the
 * Cloudflare tunnel.
 *
 * ## Errors
 *
 * * [AuthClientArgError] — caller-side wiring bug (empty password,
 *   wrong-length account-id). Surface as a form validation message.
 * * [AuthHttpError] — server returned a non-2xx, or `ok=false` on
 *   a `/finish` endpoint. Status + truncated body are exposed for
 *   diagnostics, plus the body's `error` code and the wait a `429`
 *   states (ADR-0054 S4; a `429` is never a sign-out).
 * * [AuthNetworkError] — transport failure (DNS, TLS, connection
 *   refused). Distinct from HTTP errors so the UI can suggest
 *   "check connection" rather than "wrong credentials".
 */
public class AuthClient(
    /** Base URL. No trailing slash. */
    public val baseUrl: String = DEFAULT_BASE_URL,
    /** Ktor engine. Production: `OkHttp` (default below). Tests:
     *  `MockEngine` for deterministic responses without a network. */
    engine: HttpClientEngine? = null,
    /** The signup proof-of-work solver. Tests pass a fake (no native lib). */
    private val powSolver: PowSolver = UniffiPowSolver,
) : AutoCloseable {

    /**
     * The shared JSON codec. `explicitNulls = false` matches the
     * TS side: optional fields with `null` value are omitted from
     * the wire, exactly like the spread-operator-omitted-field
     * shape `apps/web` uses. `ignoreUnknownKeys = true` keeps us
     * forward-compatible with future server fields we don't know
     * about yet — the server promise is to only **add** fields
     * (the wire format is append-only), never to rename or remove.
     */
    private val json = Json {
        ignoreUnknownKeys = true
        explicitNulls = false
    }

    /**
     * The HTTP client. Constructed once per `AuthClient` lifetime.
     * Pluggable engine so tests can drop a `MockEngine` in here
     * without round-tripping through OkHttp + a fake server.
     */
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

    // -----------------------------------------------------------------
    // Low-level: 1:1 HTTP endpoints
    // -----------------------------------------------------------------

    /** `POST /pow/challenge`. Since ADR-0054 S4 only signup start spends one. */
    public suspend fun powChallenge(): PowChallengeResponse =
        postNoBody("/pow/challenge")

    /** `POST /signup/start`. */
    public suspend fun signupStart(req: SignupStartRequest): SignupStartResponse =
        post("/signup/start", req)

    /** `POST /signup/finish`. */
    public suspend fun signupFinish(req: SignupFinishRequest): SignupFinishResponse =
        post("/signup/finish", req)

    /** `POST /login/start`. */
    public suspend fun loginStart(req: LoginStartRequest): LoginStartResponse =
        post("/login/start", req)

    /** `POST /login/finish`. */
    public suspend fun loginFinish(req: LoginFinishRequest): LoginFinishResponse =
        post("/login/finish", req)

    /** `POST /recovery/login/start`. */
    public suspend fun recoveryLoginStart(req: LoginStartRequest): LoginStartResponse =
        post("/recovery/login/start", req)

    /** `POST /recovery/login/finish`. */
    public suspend fun recoveryLoginFinish(req: LoginFinishRequest): LoginFinishResponse =
        post("/recovery/login/finish", req)

    /** `POST /password/reset/start`. */
    public suspend fun passwordResetStart(
        req: PasswordResetStartRequest,
    ): PasswordResetStartResponse =
        post("/password/reset/start", req)

    /** `POST /password/reset/finish`. */
    public suspend fun passwordResetFinish(
        req: PasswordResetFinishRequest,
    ): PasswordResetFinishResponse =
        post("/password/reset/finish", req)

    // -----------------------------------------------------------------
    // Device authorization — ADR-0018 factor (a), "another-device"
    // recovery
    // -----------------------------------------------------------------

    /**
     * `POST /device/authorize/start` — mint a single-use 32-byte
     * device-authorization grant for this account (ADR-0018 factor
     * (a), scanner side). Mirror of the web
     * `AuthClient.deviceAuthorizeStart`.
     *
     * Bearer-authenticated (the EXISTING device proves it already
     * holds a session); the request has no body. Returns the URL-safe base64
     * (no pad) of the RAW grant token — the server stores only its
     * SHA-256. The caller seals it (together with the phrase seed)
     * to the fresh device over the E2EE pairing channel; it never
     * travels in plaintext past this response.
     */
    public suspend fun deviceAuthorizeStart(sessionToken: String): String {
        if (sessionToken.isEmpty()) {
            throw AuthClientArgError("sessionToken must be non-empty")
        }
        val resp: DeviceAuthorizeStartResponse =
            postNoBody("/device/authorize/start", bearer = sessionToken)
        return resp.grant_token_b64
    }

    /**
     * `POST /device/authorize/redeem` — atomically consume a
     * single-use device grant for a session-token triple (ADR-0018
     * factor (a), fresh-device side). Mirror of the web
     * `AuthClient.deviceAuthorizeRedeem`.
     *
     * Anonymous (the fresh device has no token yet). A
     * missing / expired / already-consumed grant fails as an
     * enumeration-safe 401 ([AuthHttpError]), indistinguishable from
     * a wrong credential by design.
     *
     * @param accountId      Raw 16-byte account id from the pairing QR.
     * @param grantTokenB64  URL-safe base64 (no pad) of the raw
     *                       32-byte grant token received over the
     *                       rendezvous channel.
     */
    public suspend fun deviceAuthorizeRedeem(
        accountId: ByteArray,
        grantTokenB64: String,
    ): DeviceAuthorizeRedeemResult {
        if (accountId.size != ACCOUNT_ID_LEN) {
            throw AuthClientArgError("accountId must be $ACCOUNT_ID_LEN bytes")
        }
        if (grantTokenB64.isEmpty()) {
            throw AuthClientArgError("grantTokenB64 must be non-empty")
        }
        val resp: DeviceAuthorizeRedeemResponse = post(
            "/device/authorize/redeem",
            DeviceAuthorizeRedeemRequest(
                account_id_b64 = base64UrlEncodeNoPad(accountId),
                grant_token_b64 = grantTokenB64,
            ),
        )
        return DeviceAuthorizeRedeemResult(
            accountId = base64UrlDecodeNoPad(resp.account_id_b64),
            sessionToken = resp.session_token,
            sessionExpiresAtMs = resp.session_expires_at_ms,
        )
    }

    // -----------------------------------------------------------------
    // Auth-attested KT-log genesis — ADR-0036 (G-30 finding 2)
    // -----------------------------------------------------------------

    /**
     * `POST /auth/genesis/attest` — mint an auth-attested genesis
     * attestation binding the OPAQUE-authenticated account (proven by
     * [sessionToken]) to its client-supplied genesis identity [pubkey]
     * (ADR-0036, G-30 finding 2). Mirror of the web / iOS mint.
     *
     * Bearer-authenticated, NO PoW — holding a valid session token IS the
     * proof the caller OPAQUE-authenticated as the account, which is the
     * whole security basis: only the account holder can mint an attestation
     * for their own account, so a squatter cannot obtain one for a victim's
     * account. Returns the RAW 153-byte `GenesisAttestation` (decoded from
     * the server's URL-safe base64), ready to ride onto the identity-bundle
     * upload alongside the ADR-0019 genesis self-signature.
     */
    public suspend fun genesisAttest(sessionToken: String, pubkey: ByteArray): ByteArray {
        if (sessionToken.isEmpty()) {
            throw AuthClientArgError("sessionToken must be non-empty")
        }
        if (pubkey.size != PUBKEY_LEN) {
            throw AuthClientArgError("pubkey must be $PUBKEY_LEN bytes (Ed25519)")
        }
        val resp: GenesisAttestResponse = post(
            "/auth/genesis/attest",
            GenesisAttestRequest(pubkey_b64 = base64UrlEncodeNoPad(pubkey)),
            bearer = sessionToken,
        )
        return base64UrlDecodeNoPad(resp.genesis_attest_b64)
    }

    // -----------------------------------------------------------------
    // TOTP — ADR-0018 factor (c), authenticator-code recovery
    // -----------------------------------------------------------------

    /**
     * `POST /totp/enroll/start` — mint + store an (unconfirmed) TOTP
     * secret for this account (ADR-0018 factor (c), enrollment step
     * 1). Mirror of the web `AuthClient.totpEnrollStart`.
     *
     * Bearer-authenticated (the already-logged-in account); no body.
     * Returns the `otpauth://` provisioning URI (QR) + the base32
     * secret (manual entry). The enrollment only becomes usable for
     * recovery after [totpEnrollFinish] confirms it with a live code.
     *
     * A 409 [AuthHttpError] means a CONFIRMED authenticator already
     * exists (the server refuses a silent swap); a 503 means the
     * server has TOTP disabled (`LAKKO_TOTP_ENC_KEY` unset).
     */
    public suspend fun totpEnrollStart(sessionToken: String): TotpEnrollStartResult {
        if (sessionToken.isEmpty()) {
            throw AuthClientArgError("sessionToken must be non-empty")
        }
        val resp: TotpEnrollStartResponse =
            postNoBody("/totp/enroll/start", bearer = sessionToken)
        return TotpEnrollStartResult(
            otpauthUri = resp.otpauth_uri,
            secretB32 = resp.secret_b32,
        )
    }

    /**
     * `POST /totp/enroll/finish` — confirm the enrollment with the
     * first code the authenticator produced (ADR-0018 factor (c),
     * enrollment step 2). Mirror of the web
     * `AuthClient.totpEnrollFinish`.
     *
     * Bearer-authenticated, NO PoW — the server does not gate
     * finish. A 401 [AuthHttpError] means the code did not verify
     * (wrong app entry / clock skew beyond +-30 s); the pending
     * enrollment stays pending, so the user can simply retry with
     * the next code.
     */
    public suspend fun totpEnrollFinish(sessionToken: String, code: String) {
        if (sessionToken.isEmpty()) {
            throw AuthClientArgError("sessionToken must be non-empty")
        }
        val normalized = normalizeTotpCode(code)
            ?: throw AuthClientArgError("code must be 6 digits")
        // The body is `{"enabled": true}` — nothing the caller
        // consumes; a typed JsonObject keeps the post helper's
        // non-null contract without inventing a DTO for it.
        post<TotpEnrollFinishRequest, JsonObject>(
            "/totp/enroll/finish",
            TotpEnrollFinishRequest(code = normalized),
            bearer = sessionToken,
        )
    }

    /**
     * `POST /totp/recover` — recover ACCESS with an authenticator
     * code (ADR-0018 factor (c), recovery side). Mirror of the web
     * `AuthClient.totpRecover`.
     *
     * Anonymous (the caller lost their device). The server is
     * enumeration-safe by design: unknown account, unconfirmed
     * enrollment, wrong code, and replayed code all surface the SAME
     * generic 401 [AuthHttpError]; 429 `too-many-attempts` signals the
     * per-name limit (charged identically for any name); 503 means
     * TOTP is disabled server-side.
     *
     * TOTP grants ACCESS only — the result carries no key material.
     * Identity acquisition is the caller's job (ADR-0017 state
     * machine in [AuthRepository.recoverWithTotp]).
     *
     * ADR-0054 S5: the account is named by its 32-byte login [handle]; the
     * account id is learned only from this response.
     */
    public suspend fun totpRecover(
        handle: ByteArray,
        code: String,
    ): TotpRecoverResult {
        requireHandle(handle)
        val normalized = normalizeTotpCode(code)
            ?: throw AuthClientArgError("code must be 6 digits")
        val resp: TotpRecoverResponse = post(
            "/totp/recover",
            TotpRecoverRequest(
                handle = base64UrlEncodeNoPad(handle),
                code = normalized,
            ),
        )
        return TotpRecoverResult(
            accountId = accountIdFromFinish(base64UrlDecodeNoPad(resp.account_id_b64), "/totp/recover"),
            sessionToken = resp.session_token,
            sessionExpiresAtMs = resp.session_expires_at_ms,
        )
    }

    // -----------------------------------------------------------------
    // Recovery contact — ADR-0018 factor (d), phone / email OTP
    // recovery
    // -----------------------------------------------------------------

    /**
     * `POST /recovery-contact/enroll/start` — store a phone / email
     * and send a 6-digit confirmation code (ADR-0018 factor (d),
     * enrollment step 1). Mirror of the web
     * `AuthClient.recoveryContactEnrollStart`.
     *
     * Bearer-authenticated (the already-logged-in account's session
     * token rides the `Authorization` header). The enrollment
     * only becomes usable for recovery after [recoveryContactEnrollFinish]
     * confirms the code.
     *
     * A 503 [AuthHttpError] means the server has the factor disabled
     * (`LAKKO_RECOVERY_CONTACT_PEPPER` unset); a 400 means the contact
     * is malformed per the server's normalization.
     *
     * @param kind     [ContactKind.EMAIL] (1) or [ContactKind.PHONE] (2).
     * @param contact  The raw phone / email plaintext; trimmed before
     *                 send. Used transiently server-side to hash +
     *                 address the message; never stored (invariant #1).
     */
    public suspend fun recoveryContactEnrollStart(
        sessionToken: String,
        kind: Int,
        contact: String,
    ) {
        if (sessionToken.isEmpty()) {
            throw AuthClientArgError("sessionToken must be non-empty")
        }
        if (contact.trim().isEmpty()) {
            throw AuthClientArgError("contact must be non-empty")
        }
        // The body is `{"sent": true}` — a typed response keeps the
        // post helper's non-null contract; the caller ignores it (the
        // enroll path has no enumeration concern).
        post<RecoveryContactEnrollStartRequest, RecoveryContactSentResponse>(
            "/recovery-contact/enroll/start",
            RecoveryContactEnrollStartRequest(
                kind = kind,
                contact = contact.trim(),
            ),
            bearer = sessionToken,
        )
    }

    /**
     * `POST /recovery-contact/enroll/finish` — confirm the contact
     * with the code that was delivered (ADR-0018 factor (d),
     * enrollment step 2). Mirror of the web
     * `AuthClient.recoveryContactEnrollFinish`.
     *
     * Bearer-authenticated, NO PoW — the server does not gate finish.
     * A 401 [AuthHttpError] means the code did not verify (wrong /
     * expired / replayed); the user can retry by re-sending a code.
     */
    public suspend fun recoveryContactEnrollFinish(
        sessionToken: String,
        kind: Int,
        code: String,
    ) {
        if (sessionToken.isEmpty()) {
            throw AuthClientArgError("sessionToken must be non-empty")
        }
        val normalized = normalizeTotpCode(code)
            ?: throw AuthClientArgError("code must be 6 digits")
        post<RecoveryContactEnrollFinishRequest, JsonObject>(
            "/recovery-contact/enroll/finish",
            RecoveryContactEnrollFinishRequest(kind = kind, code = normalized),
            bearer = sessionToken,
        )
    }

    /**
     * `POST /recovery-contact/recover/start` — request a fresh
     * recovery code be sent to a previously-confirmed contact
     * (ADR-0018 factor (d), recovery step 1). Mirror of the web
     * `AuthClient.recoveryContactRecoverStart`.
     *
     * Anonymous (the caller lost their device). The server is
     * ENUMERATION-SAFE by design: it returns the same `{ sent: true }`
     * whether or not the supplied contact matches a confirmed row, so
     * a non-throwing return here reveals nothing about account /
     * contact existence — it just means "advance to the code step".
     * A 503 means the factor is disabled server-side; a 400 means a
     * malformed contact.
     */
    public suspend fun recoveryContactRecoverStart(
        handle: ByteArray,
        kind: Int,
        contact: String,
    ) {
        requireHandle(handle)
        if (contact.trim().isEmpty()) {
            throw AuthClientArgError("contact must be non-empty")
        }
        post<RecoveryContactRecoverStartRequest, RecoveryContactSentResponse>(
            "/recovery-contact/recover/start",
            RecoveryContactRecoverStartRequest(
                handle = base64UrlEncodeNoPad(handle),
                kind = kind,
                contact = contact.trim(),
            ),
        )
    }

    /**
     * `POST /recovery-contact/recover/finish` — recover ACCESS with
     * the code that was delivered (ADR-0018 factor (d), recovery
     * step 2). Mirror of the web `AuthClient.recoveryContactRecoverFinish`.
     *
     * Anonymous. Enumeration-safe: unknown account, no live
     * challenge, wrong code and replayed code all surface the SAME
     * generic 401 [AuthHttpError]; 429 signals the attempt-cap
     * lockout; 503 means the factor is disabled server-side.
     *
     * Grants ACCESS only — the result carries no key material.
     * Identity acquisition is the caller's job (ADR-0017 state
     * machine in [AuthRepository.recoverWithContactFinish]).
     */
    public suspend fun recoveryContactRecoverFinish(
        handle: ByteArray,
        kind: Int,
        code: String,
    ): RecoveryContactRecoverResult {
        requireHandle(handle)
        val normalized = normalizeTotpCode(code)
            ?: throw AuthClientArgError("code must be 6 digits")
        val resp: RecoveryContactRecoverFinishResponse = post(
            "/recovery-contact/recover/finish",
            RecoveryContactRecoverFinishRequest(
                handle = base64UrlEncodeNoPad(handle),
                kind = kind,
                code = normalized,
            ),
        )
        return RecoveryContactRecoverResult(
            accountId = accountIdFromFinish(
                base64UrlDecodeNoPad(resp.account_id_b64),
                "/recovery-contact/recover/finish",
            ),
            sessionToken = resp.session_token,
            sessionExpiresAtMs = resp.session_expires_at_ms,
        )
    }

    // -----------------------------------------------------------------
    // High-level: full signup / login / recover flows
    // -----------------------------------------------------------------

    /**
     * Drive the full signup flow:
     *
     * ```text
     *   POST /pow/challenge   →  seed
     *   client: solve PoW (off the main thread, cancellable)
     *   client: OPAQUE password start [+ recovery start]
     *   POST /signup/start    →  password_response [+ recovery_response]
     *   client: OPAQUE finish (both flows)
     *   POST /signup/finish   →  ok
     * ```
     *
     * ADR-0054 S4: the start's proof is the ONE proof of work left in the
     * app, and the finish carries none; it must follow a start for the same
     * account within 600 s. A finish refused with `410` (the start expired)
     * restarts the whole signup once, from a fresh challenge and a fresh
     * account id.
     *
     * [onProofSolved] runs once the proof is ready, before anything is sent:
     * up to then cancelling leaves nothing on the server (the UX `Cancel`).
     *
     * When [enrolRecovery] is `true` (the default), the method also
     * generates a fresh BIP39 24-word mnemonic, enrols it as a
     * parallel OPAQUE envelope under `KIND_RECOVERY` server-side,
     * and returns the phrase in [SignupResult.recoveryPhrase] for
     * the UI to display **once**. Both envelopes commit atomically
     * inside `/signup/finish` (server transaction).
     *
     * Throws [AuthClientArgError] on an empty password.
     * [AuthHttpError] on a non-2xx response and [AuthNetworkError]
     * on a transport failure.
     */
    public suspend fun signup(
        password: ByteArray,
        enrolRecovery: Boolean = true,
        onProofSolved: () -> Unit = {},
    ): SignupResult {
        if (password.isEmpty()) {
            throw AuthClientArgError("password must be non-empty")
        }
        return try {
            signupOnce(password, enrolRecovery, onProofSolved)
        } catch (e: AuthHttpError) {
            if (e.status != HTTP_GONE || e.endpoint != "/signup/finish") throw e
            signupOnce(password, enrolRecovery, onProofSolved)
        }
    }

    private suspend fun signupOnce(
        password: ByteArray,
        enrolRecovery: Boolean,
        onProofSolved: () -> Unit,
    ): SignupResult {
        val accountId = generateAccountId()
        val accountIdB64 = base64Encode(accountId)

        // ------------- start
        val challenge = powChallenge()
        val pow = solvePow(challenge.seed, challenge.effort_bits, powSolver)
        onProofSolved()

        // Password OPAQUE start. UniFFI handles are AutoCloseable
        // — `use` guarantees Rust-side cleanup on every code path,
        // including exceptions thrown by the network call below.
        OpaqueRegister.startPassword(password).use { passwordReg ->
            val recovery: RecoveryBundle? = if (enrolRecovery) {
                RecoveryMnemonic.generate().use { mnemonic ->
                    val seed = mnemonic.toSeed()
                    RecoveryBundle(
                        mnemonicPhrase = mnemonic.phrase(),
                        seed = seed,
                        opaque = OpaqueRegister.startRecovery(seed),
                    )
                }
            } else {
                null
            }

            try {
                val startResp = signupStart(
                    SignupStartRequest(
                        pow = pow,
                        account_id = accountIdB64,
                        password_request = passwordReg.requestB64(),
                        recovery_request = recovery?.opaque?.requestB64(),
                    ),
                )

                if (recovery != null && startResp.recovery_response == null) {
                    // The server should always echo back a recovery_response
                    // when we set recovery_request. Treat the missing field as
                    // a server-side wiring bug rather than crashing later in
                    // OPAQUE finish.
                    throw AuthHttpError(
                        status = 200,
                        endpoint = "/signup/start",
                        body = "missing recovery_response despite recovery_request being set",
                    )
                }

                // ------------- finish (OPAQUE math)
                val passwordOut = passwordReg.finish(password, startResp.password_response)
                try {
                    val recoveryUploadB64: String? =
                        if (recovery != null && startResp.recovery_response != null) {
                            recovery.opaque.finish(recovery.seed, startResp.recovery_response)
                                .use { it.uploadB64() }
                        } else {
                            null
                        }

                    val finishResp = signupFinish(
                        SignupFinishRequest(
                            account_id = accountIdB64,
                            password_upload = passwordOut.uploadB64(),
                            recovery_upload = recoveryUploadB64,
                        ),
                    )
                    if (!finishResp.ok) {
                        throw AuthHttpError(200, "/signup/finish", "server reported ok=false")
                    }

                    return SignupResult(
                        accountId = accountId,
                        exportKey = passwordOut.exportKey(),
                        recoveryPhrase = recovery?.mnemonicPhrase,
                        sessionToken = finishResp.session_token,
                        sessionExpiresAtMs = finishResp.session_expires_at_ms,
                    )
                } finally {
                    passwordOut.close()
                }
            } finally {
                recovery?.opaque?.close()
            }
        }
    }

    /**
     * Drive the full password-login flow:
     *
     * ```text
     *   client: OPAQUE start
     *   POST /login/start    →  session_id, response
     *   client: OPAQUE finish
     *   POST /login/finish   →  ok, account_id
     * ```
     *
     * ADR-0054 S5: the account is named by its 32-byte login [handle]
     * (`loginHandle(username)`), never by its id; the account id is
     * learned from `/login/finish`, after the proof. No discovery lookup
     * runs before a session.
     *
     * Wrong passwords surface as an [AuthHttpError] with status
     * 401 — by design, indistinguishable from "unknown account" (an
     * unknown handle takes the server's ordinary path on a stand-in
     * account).
     */
    public suspend fun login(
        handle: ByteArray,
        password: ByteArray,
    ): LoginResult {
        if (password.isEmpty()) {
            throw AuthClientArgError("password must be non-empty")
        }
        requireHandle(handle)

        OpaqueLogin.startPassword(password).use { login ->
            val startResp = loginStart(
                LoginStartRequest(
                    handle = base64UrlEncodeNoPad(handle),
                    request = login.requestB64(),
                ),
            )

            val out = login.finish(password, startResp.response)
            try {
                val finishResp = loginFinish(
                    LoginFinishRequest(
                        session_id = startResp.session_id,
                        finalize = out.finalizeB64(),
                    ),
                )
                if (!finishResp.ok) {
                    throw AuthHttpError(200, "/login/finish", "server reported ok=false")
                }
                return LoginResult(
                    accountId = accountIdFromFinish(base64Decode(finishResp.account_id), "/login/finish"),
                    sessionKey = out.sessionKey(),
                    exportKey = out.exportKey(),
                    sessionToken = finishResp.session_token,
                    sessionExpiresAtMs = finishResp.session_expires_at_ms,
                )
            } finally {
                out.close()
            }
        }
    }

    /**
     * Drive the full recover-from-mnemonic flow:
     *
     * ```text
     *   client: OPAQUE recovery start (seed)
     *   client: OPAQUE password start (newPassword)
     *   POST /password/reset/start   →  session_id, recovery_response,
     *                                    new_password_response
     *   client: OPAQUE recovery finish + OPAQUE password finish
     *   POST /password/reset/finish  →  ok, account_id
     * ```
     *
     * Drives the `/password/reset/{start,finish}` endpoint pair.
     * The server proves recovery ownership via the mnemonic and
     * atomically replaces the `KIND_PASSWORD` envelope with the
     * new one — the `KIND_RECOVERY` envelope stays intact, so the
     * same 24 words work for future resets.
     *
     * No proof of work (ADR-0054 S4): the server's per-name and
     * per-address counters bound the guesses.
     * Wrong-mnemonic attempts fail at the AKE finalization with
     * the same indistinguishable response as `/recovery/login/finish`.
     *
     * @param handle       The 32-byte login handle of the account
     *                     (ADR-0054 S5); the account id comes back in
     *                     `/password/reset/finish`.
     * @param mnemonicSeed The 64-byte BIP39 seed derived from the
     *                     user's typed phrase via
     *                     `RecoveryMnemonic.fromPhrase(phrase).toSeed()`.
     *                     Caller is responsible for surfacing
     *                     mnemonic-parsing errors before reaching
     *                     this method.
     * @param newPassword  The replacement password as raw UTF-8 bytes.
     */
    public suspend fun recover(
        handle: ByteArray,
        mnemonicSeed: ByteArray,
        newPassword: ByteArray,
    ): RecoverResult {
        requireHandle(handle)
        if (mnemonicSeed.isEmpty()) {
            throw AuthClientArgError("mnemonicSeed must be non-empty")
        }
        if (newPassword.isEmpty()) {
            throw AuthClientArgError("newPassword must be non-empty")
        }

        // Two parallel OPAQUE flows: recovery login proves the user
        // knows the mnemonic; password registration enrols the new
        // password envelope. Both are independent — neither needs
        // the other's intermediate state.
        OpaqueLogin.startRecovery(mnemonicSeed).use { recoveryLogin ->
            OpaqueRegister.startPassword(newPassword).use { newPasswordReg ->
                val startResp = passwordResetStart(
                    PasswordResetStartRequest(
                        handle = base64UrlEncodeNoPad(handle),
                        recovery_request = recoveryLogin.requestB64(),
                        new_password_request = newPasswordReg.requestB64(),
                    ),
                )

                // Both finish steps. A wrong mnemonic seed here
                // surfaces as an OPAQUE-side error at
                // `recoveryLogin.finish` (the AKE can't agree on a
                // session key); we surface it to the caller
                // unchanged so the form can render "invalid recovery
                // phrase" without distinguishing further.
                val recoveryOut = recoveryLogin.finish(mnemonicSeed, startResp.recovery_response)
                try {
                    val passwordOut = newPasswordReg.finish(
                        newPassword,
                        startResp.new_password_response,
                    )
                    try {
                        val finishResp = passwordResetFinish(
                            PasswordResetFinishRequest(
                                session_id = startResp.session_id,
                                recovery_finalize = recoveryOut.finalizeB64(),
                                new_password_upload = passwordOut.uploadB64(),
                            ),
                        )
                        if (!finishResp.ok) {
                            throw AuthHttpError(
                                200,
                                "/password/reset/finish",
                                "server reported ok=false",
                            )
                        }
                        return RecoverResult(
                            accountId = accountIdFromFinish(
                                base64Decode(finishResp.account_id),
                                "/password/reset/finish",
                            ),
                            exportKey = passwordOut.exportKey(),
                            sessionToken = finishResp.session_token,
                            sessionExpiresAtMs = finishResp.session_expires_at_ms,
                        )
                    } finally {
                        passwordOut.close()
                    }
                } finally {
                    recoveryOut.close()
                }
            }
        }
    }

    // -----------------------------------------------------------------
    // Login handle - ADR-0054 S5
    // -----------------------------------------------------------------

    /**
     * `PUT /auth/login-handle` - bind the signed-in account to [handle]
     * (replacing its previous handle). Idempotent for a handle the account
     * already holds.
     *
     * @return [LoginHandleBinding.Taken] when another account holds the
     *   handle (`409 handle-taken`; nothing changed server-side).
     * @throws AuthHttpError on any other refusal (`503
     *   login-by-name-disabled` included).
     */
    public suspend fun bindLoginHandle(sessionToken: String, handle: ByteArray): LoginHandleBinding {
        if (sessionToken.isEmpty()) {
            throw AuthClientArgError("sessionToken must be non-empty")
        }
        requireHandle(handle)
        return try {
            sendJson<LoginHandleRequest, LoginHandleBoundResponse>(
                HttpMethod.Put,
                LOGIN_HANDLE_PATH,
                LoginHandleRequest(handle = base64UrlEncodeNoPad(handle)),
                bearer = sessionToken,
            )
            LoginHandleBinding.Bound
        } catch (e: AuthHttpError) {
            if (e.status == HTTP_CONFLICT) LoginHandleBinding.Taken else throw e
        }
    }

    /** `DELETE /auth/login-handle` - remove the signed-in account's handle.
     *  @return whether a handle was removed. */
    public suspend fun unbindLoginHandle(sessionToken: String): Boolean {
        if (sessionToken.isEmpty()) {
            throw AuthClientArgError("sessionToken must be non-empty")
        }
        val resp: LoginHandleRemovedResponse = doRequest(HttpMethod.Delete, LOGIN_HANDLE_PATH) {
            header(HttpHeaders.Authorization, "Bearer $sessionToken")
        }
        return resp.removed
    }

    private fun requireHandle(handle: ByteArray) {
        if (handle.size != LOGIN_HANDLE_LEN) {
            throw AuthClientArgError("handle must be $LOGIN_HANDLE_LEN bytes")
        }
    }

    /** The account id a finish response names; a wrong length is a server
     *  wiring fault, never trusted. */
    private fun accountIdFromFinish(accountId: ByteArray, endpoint: String): ByteArray {
        if (accountId.size != ACCOUNT_ID_LEN) {
            throw AuthHttpError(200, endpoint, "account id of ${accountId.size} bytes")
        }
        return accountId
    }

    // -----------------------------------------------------------------
    // Internal HTTP helper
    // -----------------------------------------------------------------

    /**
     * One-shot POST with a JSON body. Reified type parameters drive
     * both the body serialization and the response deserialization.
     * `Res: Any` so we don't accidentally try to deserialize into a
     * nullable shape (every endpoint returns a non-null body).
     */
    private suspend inline fun <reified Req : Any, reified Res : Any> post(
        endpoint: String,
        body: Req,
        bearer: String? = null,
    ): Res = sendJson(HttpMethod.Post, endpoint, body, bearer)

    /** One-shot request of [method] with a JSON body. */
    private suspend inline fun <reified Req : Any, reified Res : Any> sendJson(
        method: HttpMethod,
        endpoint: String,
        body: Req,
        bearer: String? = null,
    ): Res = doRequest(method, endpoint) {
        contentType(ContentType.Application.Json)
        if (bearer != null) {
            header(HttpHeaders.Authorization, "Bearer $bearer")
        }
        setBody(body)
    }

    /**
     * One-shot POST with no body: `/pow/challenge`, and the two
     * bearer routes that read none (`/totp/enroll/start`,
     * `/device/authorize/start`).
     */
    private suspend inline fun <reified Res : Any> postNoBody(
        endpoint: String,
        bearer: String? = null,
    ): Res = doRequest(HttpMethod.Post, endpoint) {
        if (bearer != null) {
            header(HttpHeaders.Authorization, "Bearer $bearer")
        }
    }

    /**
     * Shared HTTP plumbing — opens the connection, maps transport
     * errors to [AuthNetworkError], maps non-2xx responses to
     * [AuthHttpError] with the truncated server body, and
     * deserializes the response body.
     *
     * Inlined so [Res] stays reified through to `response.body()`,
     * which is the only place Ktor needs the type witness.
     */
    private suspend inline fun <reified Res : Any> doRequest(
        method: HttpMethod,
        endpoint: String,
        crossinline configure: HttpRequestBuilder.() -> Unit,
    ): Res {
        val url = "$baseUrl$endpoint"
        val response: HttpResponse = try {
            client.request(url) {
                this.method = method
                configure()
            }
        } catch (cause: Throwable) {
            throw AuthNetworkError(endpoint, cause)
        }

        if (!response.status.isSuccess()) {
            val text = response.safeReadBody()
            throw AuthHttpError(
                status = response.status.value,
                endpoint = endpoint,
                body = text,
                code = me.lakko.app.anon.errorCodeOf(text),
                retryAfterSecs = me.lakko.app.anon.longFieldOf(text, "retry_after_secs")
                    ?: response.headers[HttpHeaders.RetryAfter]?.trim()?.toLongOrNull(),
            )
        }
        return response.body()
    }

    /**
     * Carrier for the optional recovery side of [signup]. Inlining
     * a `Triple<…>` would work but the named class makes the
     * follow-up null-checks in [signup] readable at a glance.
     */
    private data class RecoveryBundle(
        val mnemonicPhrase: String,
        val seed: ByteArray,
        val opaque: OpaqueRegister,
    )

    public companion object {
        /**
         * `<LAKKO_API_BASE_URL>/auth` ([ApiEndpoints]). With no API
         * base URL in the build it is `http://10.0.2.2:8081` —
         * `10.0.2.2` is the canonical "host machine from inside the
         * Android emulator" loopback alias, port 8081 the
         * `lakko-svc-auth` dev bind.
         */
        public val DEFAULT_BASE_URL: String
            get() = ApiEndpoints.baseUrlFor(ApiService.AUTH)

        private const val ACCOUNT_ID_LEN: Int = 16

        /** Ed25519 identity pubkey length (ADR-0036 genesis-attest mint). */
        private const val PUBKEY_LEN: Int = 32

        /** `/signup/finish` without a live start (ADR-0054 S4). */
        private const val HTTP_GONE: Int = 410

        /** `PUT /auth/login-handle` for a handle another account holds. */
        private const val HTTP_CONFLICT: Int = 409

        /** ADR-0054 S5 login handle length. */
        public const val LOGIN_HANDLE_LEN: Int = 32

        private const val LOGIN_HANDLE_PATH: String = "/auth/login-handle"
    }
}

private suspend fun HttpResponse.safeReadBody(): String {
    return try {
        val text = bodyAsText()
        if (text.length > 512) "${text.take(512)}…" else text
    } catch (cause: Throwable) {
        "<unreadable body: ${cause::class.simpleName}>"
    }
}

// -----------------------------------------------------------------
// Result types
// -----------------------------------------------------------------

/** Result of [AuthClient.signup]. */
public data class SignupResult(
    /** Raw 16 bytes — the canonical id the server returned and
     *  that the client must use on every subsequent login. */
    val accountId: ByteArray,
    /** OPAQUE export key bytes (64 for `LakkoCipherSuite`). The
     *  device-root key gets derived from this in 1.15d3 — the
     *  caller MUST handle it carefully (no logging, no
     *  `SharedPreferences`). */
    val exportKey: ByteArray,
    /** The 24-word recovery phrase, present when signup was called
     *  with `enrolRecovery = true` (the default). The UI MUST
     *  display it exactly once and ask the user to record it
     *  somewhere safe — losing it means losing the only way back
     *  into the account if the password is forgotten. */
    val recoveryPhrase: String? = null,
    /** Phase 1.5b: HMAC-SHA256 bearer token. Stash in
     *  [AuthRepository.identitySecrets] then thread through every
     *  protected `lakko-svc-discovery` write via the
     *  `Authorization: Bearer <token>` header. */
    val sessionToken: String,
    /** Token expiry as a unix-millisecond timestamp. */
    val sessionExpiresAtMs: Long,
) {
    // `data class` synthesizes `equals` / `hashCode` over reference
    // equality for arrays, which is rarely what you want. The auth
    // result is not meant to be put in sets or maps; we override
    // here so any accidental `==` produces a useful answer rather
    // than a confusing one.
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is SignupResult) return false
        return accountId.contentEquals(other.accountId) &&
            exportKey.contentEquals(other.exportKey) &&
            recoveryPhrase == other.recoveryPhrase &&
            sessionToken == other.sessionToken &&
            sessionExpiresAtMs == other.sessionExpiresAtMs
    }

    override fun hashCode(): Int {
        var result = accountId.contentHashCode()
        result = 31 * result + exportKey.contentHashCode()
        result = 31 * result + (recoveryPhrase?.hashCode() ?: 0)
        result = 31 * result + sessionToken.hashCode()
        result = 31 * result + sessionExpiresAtMs.hashCode()
        return result
    }
}

/** Result of [AuthClient.login]. */
public data class LoginResult(
    val accountId: ByteArray,
    /** Symmetric session key shared with the server. Use to bind a
     *  follow-up session token; **not** to encrypt user data. */
    val sessionKey: ByteArray,
    /** Same export key the user got at signup time — deterministic
     *  for the same password. */
    val exportKey: ByteArray,
    /** Phase 1.5b: HMAC-SHA256 bearer token. */
    val sessionToken: String,
    /** Token expiry as a unix-millisecond timestamp. */
    val sessionExpiresAtMs: Long,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is LoginResult) return false
        return accountId.contentEquals(other.accountId) &&
            sessionKey.contentEquals(other.sessionKey) &&
            exportKey.contentEquals(other.exportKey) &&
            sessionToken == other.sessionToken &&
            sessionExpiresAtMs == other.sessionExpiresAtMs
    }

    override fun hashCode(): Int {
        var result = accountId.contentHashCode()
        result = 31 * result + sessionKey.contentHashCode()
        result = 31 * result + exportKey.contentHashCode()
        result = 31 * result + sessionToken.hashCode()
        result = 31 * result + sessionExpiresAtMs.hashCode()
        return result
    }
}

/** Result of [AuthClient.recover]. */
public data class RecoverResult(
    val accountId: ByteArray,
    /** OPAQUE export key for the **new** password. Derive a fresh
     *  device root key — the previous device key (if any) is no
     *  longer reachable from the new password. */
    val exportKey: ByteArray,
    /** Phase 1.5b: fresh bearer token for the rotated password. */
    val sessionToken: String,
    /** Token expiry as a unix-millisecond timestamp. */
    val sessionExpiresAtMs: Long,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is RecoverResult) return false
        return accountId.contentEquals(other.accountId) &&
            exportKey.contentEquals(other.exportKey) &&
            sessionToken == other.sessionToken &&
            sessionExpiresAtMs == other.sessionExpiresAtMs
    }

    override fun hashCode(): Int {
        var result = accountId.contentHashCode()
        result = 31 * result + exportKey.contentHashCode()
        result = 31 * result + sessionToken.hashCode()
        result = 31 * result + sessionExpiresAtMs.hashCode()
        return result
    }
}

/** Result of [AuthClient.deviceAuthorizeRedeem] — the same
 *  `(account_id, session_token, expiry)` triple every other login
 *  flow returns (ADR-0018 factor (a), fresh-device side). No
 *  exportKey: this path has no password, so no OPAQUE export key
 *  exists on this device until the user later sets one. */
public data class DeviceAuthorizeRedeemResult(
    val accountId: ByteArray,
    val sessionToken: String,
    val sessionExpiresAtMs: Long,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is DeviceAuthorizeRedeemResult) return false
        return accountId.contentEquals(other.accountId) &&
            sessionToken == other.sessionToken &&
            sessionExpiresAtMs == other.sessionExpiresAtMs
    }

    override fun hashCode(): Int {
        var result = accountId.contentHashCode()
        result = 31 * result + sessionToken.hashCode()
        result = 31 * result + sessionExpiresAtMs.hashCode()
        return result
    }
}

/** Result of [AuthClient.totpEnrollStart] (ADR-0018 factor (c)) —
 *  the provisioning material the authenticator app consumes. The
 *  raw secret leaves the server exactly once, here: base32 inside
 *  the URI + standalone for manual entry. Never log or persist. */
public data class TotpEnrollStartResult(
    /** `otpauth://totp/...` provisioning URI — render as a QR
     *  (RAW TEXT, not base64-wrapped — authenticator apps parse
     *  the URI string straight out of the QR). */
    val otpauthUri: String,
    /** RFC 4648 base32 (no padding) secret for manual entry. */
    val secretB32: String,
)

/** Result of [AuthClient.totpRecover] — the same `(account_id,
 *  session_token, expiry)` triple every other login flow returns
 *  (ADR-0018 factor (c)). No exportKey: TOTP involves no password,
 *  so no OPAQUE export key exists on this device. */
public data class TotpRecoverResult(
    val accountId: ByteArray,
    val sessionToken: String,
    val sessionExpiresAtMs: Long,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is TotpRecoverResult) return false
        return accountId.contentEquals(other.accountId) &&
            sessionToken == other.sessionToken &&
            sessionExpiresAtMs == other.sessionExpiresAtMs
    }

    override fun hashCode(): Int {
        var result = accountId.contentHashCode()
        result = 31 * result + sessionToken.hashCode()
        result = 31 * result + sessionExpiresAtMs.hashCode()
        return result
    }
}

/** Result of [AuthClient.recoveryContactRecoverFinish] — the same
 *  `(account_id, session_token, expiry)` triple every other login
 *  flow returns (ADR-0018 factor (d)). No exportKey: this path has
 *  no password, so no OPAQUE export key exists on this device. */
public data class RecoveryContactRecoverResult(
    val accountId: ByteArray,
    val sessionToken: String,
    val sessionExpiresAtMs: Long,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is RecoveryContactRecoverResult) return false
        return accountId.contentEquals(other.accountId) &&
            sessionToken == other.sessionToken &&
            sessionExpiresAtMs == other.sessionExpiresAtMs
    }

    override fun hashCode(): Int {
        var result = accountId.contentHashCode()
        result = 31 * result + sessionToken.hashCode()
        result = 31 * result + sessionExpiresAtMs.hashCode()
        return result
    }
}

/** Outcome of [AuthClient.bindLoginHandle]. */
public enum class LoginHandleBinding {
    /** The account now holds the handle. */
    Bound,

    /** Another account holds it (`409 handle-taken`): the name is taken. */
    Taken,
}

// -----------------------------------------------------------------
// Errors
// -----------------------------------------------------------------

/** A precondition was violated by the caller — e.g. an empty
 *  password. Distinct from a network error so the UI can surface
 *  it as a form validation message. */
public class AuthClientArgError(message: String) : IllegalArgumentException(message)

/** A request to the auth service did not return 2xx. The original
 *  status code and (truncated) body are exposed for diagnostics. */
public class AuthHttpError(
    /** HTTP status code from the server. */
    public val status: Int,
    /** Endpoint path — useful in error toasts. */
    public val endpoint: String,
    /** Truncated server body (max 512 chars to keep stack traces
     *  tractable). */
    public val body: String,
    /** The body's `error` code (`too-many-attempts`,
     *  `signup-rate-limited`, ...), when it carried one. */
    public val code: String? = null,
    /** The wait a `429` states (`retry_after_secs` or `Retry-After`). */
    public val retryAfterSecs: Long? = null,
) : RuntimeException("auth $endpoint → $status: $body")

/** Network failure (DNS / connectivity / TLS). Distinct from
 *  [AuthHttpError] so the UI can suggest "check connection" rather
 *  than "wrong credentials". */
public class AuthNetworkError(
    public val endpoint: String,
    cause: Throwable,
) : RuntimeException("auth $endpoint network error: $cause", cause)
