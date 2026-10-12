// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import me.lakko.app.transport.lakkoHttpEngine
import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.HttpClientEngine
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.config.ApiService
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
        HttpClient(lakkoHttpEngine()) {
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
     * holds a session) and gated on [freshGrant], the fresh-auth proof of
     * the person letting the new device in (UX 17.4); without a usable one
     * the server answers `403 fresh-auth-required`. Returns the URL-safe base64
     * (no pad) of the RAW grant token — the server stores only its
     * SHA-256. The caller seals it (together with the entropy of the
     * recovery phrase) to the fresh device over the E2EE pairing channel; it never
     * travels in plaintext past this response.
     */
    public suspend fun deviceAuthorizeStart(sessionToken: String, freshGrant: String): String {
        if (sessionToken.isEmpty()) {
            throw AuthClientArgError("sessionToken must be non-empty")
        }
        if (freshGrant.isEmpty()) {
            throw AuthClientArgError("freshGrant must be non-empty")
        }
        val resp: DeviceAuthorizeStartResponse = post(
            "/device/authorize/start",
            DeviceAuthorizeStartRequest(fresh_grant = freshGrant),
            bearer = sessionToken,
        )
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
     * ADR-0056: the request carries the grant only. The server finds the
     * account by the grant's hash and hands the account id back in the
     * response, after the grant is proven; this is where a fresh device
     * first learns which account it joined.
     *
     * @param grantTokenB64  URL-safe base64 (no pad) of the raw
     *                       32-byte grant token received over the
     *                       rendezvous channel.
     */
    public suspend fun deviceAuthorizeRedeem(
        grantTokenB64: String,
        deviceId: ByteArray? = null,
    ): DeviceAuthorizeRedeemResult {
        if (grantTokenB64.isEmpty()) {
            throw AuthClientArgError("grantTokenB64 must be non-empty")
        }
        val resp: DeviceAuthorizeRedeemResponse = post(
            "/device/authorize/redeem",
            DeviceAuthorizeRedeemRequest(
                grant_token_b64 = grantTokenB64,
                device_id_b64 = deviceIdField(deviceId),
            ),
        )
        val accountId = try {
            base64UrlDecodeNoPad(resp.account_id_b64)
        } catch (_: IllegalArgumentException) {
            null
        }
        if (accountId == null || accountId.size != ACCOUNT_ID_LEN) {
            throw AuthHttpError(
                status = HTTP_OK,
                endpoint = "/device/authorize/redeem",
                body = "malformed account_id_b64",
            )
        }
        return DeviceAuthorizeRedeemResult(
            accountId = accountId,
            session = SessionPair(
                accessToken = resp.session_token,
                accessExpiresAtMs = resp.session_expires_at_ms,
                refreshToken = resp.refresh_token,
                refreshExpiresAtMs = resp.refresh_expires_at_ms,
                keyGeneration = resp.key_generation,
            ),
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
        deviceId: ByteArray? = null,
    ): TotpRecoverResult {
        requireHandle(handle)
        val normalized = normalizeTotpCode(code)
            ?: throw AuthClientArgError("code must be 6 digits")
        val resp: TotpRecoverResponse = post(
            "/totp/recover",
            TotpRecoverRequest(
                handle = base64UrlEncodeNoPad(handle),
                code = normalized,
                device_id_b64 = deviceIdField(deviceId),
            ),
        )
        return TotpRecoverResult(
            accountId = accountIdFromFinish(base64UrlDecodeNoPad(resp.account_id_b64), "/totp/recover"),
            session = SessionPair(
                accessToken = resp.session_token,
                accessExpiresAtMs = resp.session_expires_at_ms,
                refreshToken = resp.refresh_token,
                refreshExpiresAtMs = resp.refresh_expires_at_ms,
                keyGeneration = resp.key_generation,
            ),
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
     * generates a fresh BIP39 12-word mnemonic, enrols it as a
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
        /** ADR-0066 - this device's id for the account id the signup just
         *  generated (the only landing that knows its account beforehand
         *  by construction); `null` lands a session that names no device. */
        deviceIdFor: ((accountId: ByteArray) -> ByteArray?)? = null,
    ): SignupResult {
        if (password.isEmpty()) {
            throw AuthClientArgError("password must be non-empty")
        }
        return try {
            signupOnce(password, enrolRecovery, onProofSolved, deviceIdFor)
        } catch (e: AuthHttpError) {
            if (e.status != HTTP_GONE || e.endpoint != "/signup/finish") throw e
            signupOnce(password, enrolRecovery, onProofSolved, deviceIdFor)
        }
    }

    private suspend fun signupOnce(
        password: ByteArray,
        enrolRecovery: Boolean,
        onProofSolved: () -> Unit,
        deviceIdFor: ((accountId: ByteArray) -> ByteArray?)?,
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
                            device_id_b64 = deviceIdField(deviceIdFor?.invoke(accountId)),
                        ),
                    )
                    if (!finishResp.ok) {
                        throw AuthHttpError(200, "/signup/finish", "server reported ok=false")
                    }

                    return SignupResult(
                        accountId = accountId,
                        exportKey = passwordOut.exportKey(),
                        recoveryPhrase = recovery?.mnemonicPhrase,
                        session = SessionPair(
                            accessToken = finishResp.session_token,
                            accessExpiresAtMs = finishResp.session_expires_at_ms,
                            refreshToken = finishResp.refresh_token,
                            refreshExpiresAtMs = finishResp.refresh_expires_at_ms,
                            keyGeneration = finishResp.key_generation,
                        ),
                        freshGrant = finishResp.fresh_grant,
                        freshGrantExpiresAtMs = finishResp.fresh_grant_expires_at_ms,
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
        deviceId: ByteArray? = null,
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
                        device_id_b64 = deviceIdField(deviceId),
                    ),
                )
                if (!finishResp.ok) {
                    throw AuthHttpError(200, "/login/finish", "server reported ok=false")
                }
                return LoginResult(
                    accountId = accountIdFromFinish(base64Decode(finishResp.account_id), "/login/finish"),
                    sessionKey = out.sessionKey(),
                    exportKey = out.exportKey(),
                    session = SessionPair(
                        accessToken = finishResp.session_token,
                        accessExpiresAtMs = finishResp.session_expires_at_ms,
                        refreshToken = finishResp.refresh_token,
                        refreshExpiresAtMs = finishResp.refresh_expires_at_ms,
                        keyGeneration = finishResp.key_generation,
                    ),
                    freshGrant = finishResp.fresh_grant,
                    freshGrantExpiresAtMs = finishResp.fresh_grant_expires_at_ms,
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
     * same 12 words work for future resets.
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
        deviceId: ByteArray? = null,
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
                                device_id_b64 = deviceIdField(deviceId),
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
                            session = SessionPair(
                                accessToken = finishResp.session_token,
                                accessExpiresAtMs = finishResp.session_expires_at_ms,
                                refreshToken = finishResp.refresh_token,
                                refreshExpiresAtMs = finishResp.refresh_expires_at_ms,
                                keyGeneration = finishResp.key_generation,
                            ),
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

    /**
     * Sign in with the recovery credential: the OPAQUE recovery login run
     * from the 64-byte seed of the recovery phrase
     * (`/recovery/login/start|finish`). Yields the session triple and leaves
     * the password as it is. Used after the local three-word check, with the
     * seed of the phrase this device holds. Mirror of the web
     * `AuthClient.recoveryLogin`.
     *
     * A seed that is not the account's recovery credential cannot finish the
     * exchange on this side; that is reported as the `401` the server would
     * give, so callers see one refusal whichever side noticed.
     */
    public suspend fun recoveryLogin(
        handle: ByteArray,
        seed: ByteArray,
        deviceId: ByteArray? = null,
    ): TotpRecoverResult {
        requireHandle(handle)
        if (seed.isEmpty()) {
            throw AuthClientArgError("seed must be non-empty")
        }
        OpaqueLogin.startRecovery(seed).use { login ->
            val startResp = recoveryLoginStart(
                LoginStartRequest(
                    handle = base64UrlEncodeNoPad(handle),
                    request = login.requestB64(),
                ),
            )
            val out = try {
                login.finish(seed, startResp.response)
            } catch (_: uniffi.lakko_ffi_uniffi.FfiException) {
                throw AuthHttpError(
                    HTTP_UNAUTHORIZED,
                    "/recovery/login/finish",
                    "recovery credential did not match",
                )
            }
            try {
                val finishResp = recoveryLoginFinish(
                    LoginFinishRequest(
                        session_id = startResp.session_id,
                        finalize = out.finalizeB64(),
                        device_id_b64 = deviceIdField(deviceId),
                    ),
                )
                if (!finishResp.ok) {
                    throw AuthHttpError(HTTP_OK, "/recovery/login/finish", "server reported ok=false")
                }
                return TotpRecoverResult(
                    accountId = accountIdFromFinish(
                        base64Decode(finishResp.account_id),
                        "/recovery/login/finish",
                    ),
                    session = SessionPair(
                        accessToken = finishResp.session_token,
                        accessExpiresAtMs = finishResp.session_expires_at_ms,
                        refreshToken = finishResp.refresh_token,
                        refreshExpiresAtMs = finishResp.refresh_expires_at_ms,
                        keyGeneration = finishResp.key_generation,
                    ),
                )
            } finally {
                out.close()
            }
        }
    }

    /**
     * `POST /auth/session/refresh` (ADR-0065 section 3.2): trade the refresh
     * credential for a new session pair. Anonymous - the credential is the
     * authorisation - and per-address limited.
     *
     * @throws AuthHttpError `401 key-generation-superseded` (the account's
     *   keys were reset elsewhere: the device signs out and wipes), `401
     *   refresh-invalid` (expired / unknown: the sign-in screen, nothing
     *   wiped), `429`.
     */
    public suspend fun sessionRefresh(refreshToken: String): SessionPair {
        if (refreshToken.isEmpty()) {
            throw AuthClientArgError("refreshToken must be non-empty")
        }
        val resp: SessionRefreshResponse = post(
            "/auth/session/refresh",
            SessionRefreshRequest(refresh_token = refreshToken),
        )
        return SessionPair(
            accessToken = resp.session_token,
            accessExpiresAtMs = resp.session_expires_at_ms,
            refreshToken = resp.refresh_token,
            refreshExpiresAtMs = resp.refresh_expires_at_ms,
            keyGeneration = resp.key_generation,
        )
    }

    /**
     * Reset the account keys (ADR-0065 section 2; `POST
     * /account/keys/reset/{start,finish}`, bearer): one server transaction
     * that bumps the key generation, replaces the recovery credential with
     * one made from the NEW phrase [seed], deletes every passkey and device
     * grant of the account, signs every other device out, and returns the
     * key-reset attestation binding the new generation to
     * [newSignaturePubkey] together with the pair this device continues
     * with (its old pair is superseded too). The OPAQUE messages are built
     * exactly as signup builds its recovery half.
     *
     * Nothing changes server-side when `start` fails or `finish` is never
     * sent; `finish` is one transaction.
     *
     * Both calls carry [freshGrant], the fresh-auth proof a password or
     * passkey sign-in mints (UX section 17); without a usable one the
     * server answers `403 fresh-auth-required` and does nothing.
     *
     * @throws AuthHttpError `401`, `403 fresh-auth-required`, `400`, `429
     *   too-many-attempts` (with [AuthHttpError.retryAfterSecs]), `503
     *   rate-limiter-unavailable` / `key-reset-disabled`.
     */
    // -----------------------------------------------------------------
    // ADR-0066 - logout, device binding, device removal
    // -----------------------------------------------------------------

    /**
     * `POST /auth/session/logout` (bearer, no body) -> `204`. Afterwards
     * this device's bearer gets a plain `401` and its refresh credential
     * `401 refresh-invalid`; never `device-revoked`, so a logout wipes
     * nothing. A session that names no device answers `204` too and nothing
     * is recorded server-side.
     */
    public suspend fun sessionLogout(sessionToken: String) {
        requireSession(sessionToken)
        postNoContent("/auth/session/logout", bearer = sessionToken)
    }

    /**
     * `POST /auth/session/bind-device` (bearer): name [deviceId] as the
     * device of a session that landed without one. [refreshToken] is the
     * refresh credential of the same session. Returns the pair that names
     * the device; the caller drops the unbound one. `409
     * device-already-bound` when the session names another device, `401
     * device-revoked` when that device was removed.
     */
    public suspend fun bindDevice(sessionToken: String, deviceId: ByteArray, refreshToken: String): SessionPair {
        requireSession(sessionToken)
        if (refreshToken.isEmpty()) throw AuthClientArgError("refreshToken must be non-empty")
        if (deviceId.size != DEVICE_ID_LEN) throw AuthClientArgError("deviceId must be $DEVICE_ID_LEN bytes")
        val resp: SessionRefreshResponse = post(
            "/auth/session/bind-device",
            BindDeviceRequest(device_id_b64 = base64UrlEncodeNoPad(deviceId), refresh_token = refreshToken),
            bearer = sessionToken,
        )
        return SessionPair(
            accessToken = resp.session_token,
            accessExpiresAtMs = resp.session_expires_at_ms,
            refreshToken = resp.refresh_token,
            refreshExpiresAtMs = resp.refresh_expires_at_ms,
            keyGeneration = resp.key_generation,
        )
    }

    /**
     * `POST /account/devices/revoke` (bearer) -> `204`: sign ANOTHER device
     * of the account out at auth, for good. [statement] is the 138-byte
     * `device-revoke` account statement (valid 120 s: signed right before
     * sending, never stored). `403 fresh-auth-required` without a usable
     * proof; `403 account-key-required` when the account has no recorded
     * identity key or the statement is refused; `400` for a bad id or this
     * device's own.
     */
    public suspend fun revokeDevice(
        sessionToken: String,
        freshGrant: String,
        deviceId: ByteArray,
        statement: ByteArray,
    ) {
        requireSession(sessionToken)
        if (freshGrant.isEmpty()) throw AuthClientArgError("freshGrant must be non-empty")
        if (deviceId.size != DEVICE_ID_LEN) throw AuthClientArgError("deviceId must be $DEVICE_ID_LEN bytes")
        postNoContent(
            "/account/devices/revoke",
            bearer = sessionToken,
            body = DeviceRevokeRequest(
                fresh_grant = freshGrant,
                device_id_b64 = base64UrlEncodeNoPad(deviceId),
                statement_b64 = base64UrlEncodeNoPad(statement),
            ),
        )
    }

    /** The `device_id_b64` field of a landing: absent for no device. */
    private fun deviceIdField(deviceId: ByteArray?): String? {
        if (deviceId == null) return null
        if (deviceId.size != DEVICE_ID_LEN) throw AuthClientArgError("deviceId must be $DEVICE_ID_LEN bytes")
        return base64UrlEncodeNoPad(deviceId)
    }

    /** A POST whose success is a bodiless `204`. */
    private suspend inline fun <reified Req : Any> postNoContent(endpoint: String, bearer: String, body: Req) {
        expectSuccess(endpoint) {
            contentType(ContentType.Application.Json)
            header(HttpHeaders.Authorization, "Bearer $bearer")
            setBody(body)
        }
    }

    private suspend fun postNoContent(endpoint: String, bearer: String) {
        expectSuccess(endpoint) { header(HttpHeaders.Authorization, "Bearer $bearer") }
    }

    private suspend inline fun expectSuccess(endpoint: String, crossinline configure: HttpRequestBuilder.() -> Unit) {
        val response: HttpResponse = try {
            client.request("$baseUrl$endpoint") {
                this.method = HttpMethod.Post
                configure()
            }
        } catch (cause: Throwable) {
            throw AuthNetworkError(endpoint, cause)
        }
        if (response.status.isSuccess()) return
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

    public suspend fun resetAccountKeys(
        sessionToken: String,
        freshGrant: String,
        seed: ByteArray,
        newSignaturePubkey: ByteArray,
    ): KeyResetResult {
        requireSession(sessionToken)
        if (freshGrant.isEmpty()) {
            throw AuthClientArgError("freshGrant must be non-empty")
        }
        if (seed.isEmpty()) {
            throw AuthClientArgError("seed must be non-empty")
        }
        if (newSignaturePubkey.size != PUBKEY_LEN) {
            throw AuthClientArgError("newSignaturePubkey must be $PUBKEY_LEN bytes (Ed25519)")
        }
        OpaqueRegister.startRecovery(seed).use { reg ->
            val start: KeyResetStartResponse = post(
                "/account/keys/reset/start",
                KeyResetStartRequest(fresh_grant = freshGrant, recovery_request = reg.requestB64()),
                bearer = sessionToken,
            )
            val upload = reg.finish(seed, start.recovery_response).use { it.uploadB64() }
            val finish: KeyResetFinishResponse = post(
                "/account/keys/reset/finish",
                KeyResetFinishRequest(
                    fresh_grant = freshGrant,
                    recovery_upload = upload,
                    new_signature_pubkey_b64 = base64UrlEncodeNoPad(newSignaturePubkey),
                ),
                bearer = sessionToken,
            )
            val attestation = try {
                base64UrlDecodeNoPad(finish.key_reset_attest_b64)
            } catch (_: IllegalArgumentException) {
                throw AuthHttpError(HTTP_OK, "/account/keys/reset/finish", "malformed key_reset_attest_b64")
            }
            if (attestation.size != KEY_RESET_ATTESTATION_LEN) {
                throw AuthHttpError(
                    HTTP_OK,
                    "/account/keys/reset/finish",
                    "key reset attestation of ${attestation.size} bytes",
                )
            }
            return KeyResetResult(
                attestation = attestation,
                keyGeneration = finish.key_generation,
                session = SessionPair(
                    accessToken = finish.session_token,
                    accessExpiresAtMs = finish.session_expires_at_ms,
                    refreshToken = finish.refresh_token,
                    refreshExpiresAtMs = finish.refresh_expires_at_ms,
                    keyGeneration = finish.key_generation,
                ),
            )
        }
    }

    // -----------------------------------------------------------------
    // ADR-0066 part 1: taking the account back
    // -----------------------------------------------------------------

    /**
     * `POST /account/recall/status` (anonymous): whether the account can be
     * taken back from the device that holds [oldAccountSeed] (the 32-byte
     * Ed25519 seed of the account identity from before the reset), until
     * when, and how many password attempts are left. `401 recall-refused`
     * ([recallFailureOf]) means it cannot.
     */
    public suspend fun recallStatus(
        accountId: ByteArray,
        oldAccountSeed: ByteArray,
        handle: ByteArray? = null,
    ): RecallStatus {
        // ADR-0067 section 8: a device with no account id names the account
        // by login handle; the statement's account field then carries the
        // first 16 bytes of the handle.
        if (handle != null) requireHandle(handle)
        val stated = handle?.copyOfRange(0, ACCOUNT_ID_LEN) ?: accountId
        // ADR-0066 T5 item 24: a device with a wrong clock is told the
        // server's time (never "no anchor") and signs again at it, once.
        val res: RecallStatusResponse = sendWithStaleStatementRetry { signAtMs ->
            val kind = uniffi.lakko_ffi_uniffi.AccountStatementKind.RECALL_STATUS
            val statement = if (signAtMs == null) {
                uniffi.lakko_ffi_uniffi.signAccountStatement(oldAccountSeed, kind, stated, ByteArray(0))
            } else {
                uniffi.lakko_ffi_uniffi.signAccountStatementAt(oldAccountSeed, kind, stated, ByteArray(0), signAtMs)
            }
            post(
                "/account/recall/status",
                RecallStatusRequest(
                    statement_b64 = base64UrlEncodeNoPad(statement),
                    handle = handle?.let { base64UrlEncodeNoPad(it) },
                ),
            )
        }
        return recallStatusOf(res, byHandle = handle != null)
    }

    /**
     * Take the account back (`POST /account/recall/start` then `/finish`,
     * both anonymous): prove the anchor's key and the password the account
     * had BEFORE the reset, and in the same exchange set a new password, a
     * new recovery credential ([newSeed], the 64-byte seed of a new phrase)
     * and a new identity key. The answer is the key reset's: the caller
     * carries on exactly as after `reset/finish`.
     *
     * A wrong [oldPassword] is noticed on THIS device (the OPAQUE login
     * cannot finish) and `finish` is never called; it throws
     * [RecallWrongPassword], as does the server's `401 wrong-password`. Each
     * accepted `start` is one of the anchor's attempts either way.
     */
    public suspend fun recallAccount(
        accountId: ByteArray,
        oldAccountSeed: ByteArray,
        anchorSeq: Long,
        oldPassword: ByteArray,
        newPassword: ByteArray,
        newSeed: ByteArray,
        newSignaturePubkey: ByteArray,
        deviceId: ByteArray? = null,
        /** ADR-0067 section 8: the 64-byte seed of the OLD recovery phrase.
         *  When given, the login is made against the anchored RECOVERY
         *  envelope (`proof: "phrase"`) and [oldPassword] is not used. */
        oldRecoverySeed: ByteArray? = null,
    ): RecallResult {
        if ((oldRecoverySeed == null && oldPassword.isEmpty()) || newPassword.isEmpty()) {
            throw AuthClientArgError("passwords must be non-empty")
        }
        if (newSeed.isEmpty()) throw AuthClientArgError("seed must be non-empty")
        if (newSignaturePubkey.size != PUBKEY_LEN) {
            throw AuthClientArgError("newSignaturePubkey must be $PUBKEY_LEN bytes (Ed25519)")
        }
        if (anchorSeq < 0) throw AuthClientArgError("anchorSeq must not be negative")
        val accountIdB64Url = base64UrlEncodeNoPad(accountId)
        val loginSecret = oldRecoverySeed ?: oldPassword
        val loginStart =
            if (oldRecoverySeed != null) OpaqueLogin.startRecovery(oldRecoverySeed) else OpaqueLogin.startPassword(oldPassword)
        loginStart.use { login ->
            OpaqueRegister.startPassword(newPassword).use { passwordReg ->
                OpaqueRegister.startRecovery(newSeed).use { recoveryReg ->
                    val loginRequest = login.requestB64()
                    // A stale statement is told the server's time and signed
                    // again at it, once; nothing was counted (T5 item 24).
                    val start: RecallStartResponse = sendWithStaleStatementRetry { signAtMs ->
                        val startStatement = if (signAtMs == null) {
                            uniffi.lakko_ffi_uniffi.signRecallStartStatement(
                                oldAccountSeed,
                                accountId,
                                anchorSeq.toULong(),
                                loginRequest,
                            )
                        } else {
                            uniffi.lakko_ffi_uniffi.signAccountStatementAt(
                                oldAccountSeed,
                                uniffi.lakko_ffi_uniffi.AccountStatementKind.RECALL_START,
                                accountId,
                                uniffi.lakko_ffi_uniffi.recallStartStatementBody(anchorSeq.toULong(), loginRequest),
                                signAtMs,
                            )
                        }
                        post(
                            "/account/recall/start",
                            RecallStartRequest(
                                account_id_b64 = accountIdB64Url,
                                anchor_seq = anchorSeq,
                                statement_b64 = base64UrlEncodeNoPad(startStatement),
                                login_request = loginRequest,
                                password_request = passwordReg.requestB64(),
                                recovery_request = recoveryReg.requestB64(),
                                proof = if (oldRecoverySeed != null) "phrase" else null,
                            ),
                        )
                    }
                    val serverNonce = try {
                        base64UrlDecodeNoPad(start.server_nonce_b64)
                    } catch (_: IllegalArgumentException) {
                        throw AuthHttpError(HTTP_OK, "/account/recall/start", "malformed server_nonce_b64")
                    }
                    // The password check. A failure here is the OLD password
                    // being wrong; nothing more is sent.
                    val loginOut = try {
                        login.finish(loginSecret, start.login_response)
                    } catch (_: uniffi.lakko_ffi_uniffi.FfiException) {
                        throw RecallWrongPassword()
                    }
                    try {
                        val passwordUpload = passwordReg.finish(newPassword, start.password_response)
                            .use { it.uploadB64() }
                        val recoveryUpload = recoveryReg.finish(newSeed, start.recovery_response)
                            .use { it.uploadB64() }
                        val loginFinish = loginOut.finalizeB64()
                        val deviceIdB64 = deviceIdField(deviceId)
                        val pubkeyB64 = base64UrlEncodeNoPad(newSignaturePubkey)
                        fun requestSignedAt(signAtMs: Long?): RecallFinishRequest {
                            val statement = if (signAtMs == null) {
                                uniffi.lakko_ffi_uniffi.signRecallStatement(
                                    oldAccountSeed,
                                    accountId,
                                    serverNonce,
                                    newSignaturePubkey,
                                    passwordUpload,
                                    recoveryUpload,
                                )
                            } else {
                                uniffi.lakko_ffi_uniffi.signAccountStatementAt(
                                    oldAccountSeed,
                                    uniffi.lakko_ffi_uniffi.AccountStatementKind.RECALL,
                                    accountId,
                                    uniffi.lakko_ffi_uniffi.recallStatementBody(
                                        serverNonce,
                                        newSignaturePubkey,
                                        passwordUpload,
                                        recoveryUpload,
                                    ),
                                    signAtMs,
                                )
                            }
                            return RecallFinishRequest(
                                session_id = start.session_id,
                                account_id_b64 = accountIdB64Url,
                                anchor_seq = anchorSeq,
                                login_finish = loginFinish,
                                password_upload = passwordUpload,
                                recovery_upload = recoveryUpload,
                                new_signature_pubkey_b64 = pubkeyB64,
                                device_id_b64 = deviceIdB64,
                                statement_b64 = base64UrlEncodeNoPad(statement),
                            )
                        }
                        // A stale statement changed nothing and consumed
                        // nothing: sign at the server's time, once. From
                        // then on the request is fixed, and a lost answer is
                        // asked again with the SAME bytes (T5 item 25).
                        return sendWithStaleStatementRetry { signAtMs ->
                            sendRecallFinish(requestSignedAt(signAtMs))
                        }
                    } finally {
                        loginOut.close()
                    }
                }
            }
        }
    }

    /**
     * Send one `recall/finish` [request]; while no answer is read
     * ([recallFinishRetryDelayMs]) the BYTE-IDENTICAL request is sent again,
     * which the server answers `200` again with the same attestation for the
     * attempt's ten minutes. When the repeats are used up the request is
     * handed back in [RecallFinishUnanswered] so the caller keeps it, and the
     * phrase it would make the account's, and asks again later
     * ([recallFinishResend]).
     */
    private suspend fun sendRecallFinish(request: RecallFinishRequest): RecallResult {
        val firstSentAtMs = System.currentTimeMillis()
        var repeats = 0
        while (true) {
            try {
                return recallFinishOnce(request)
            } catch (cancel: kotlinx.coroutines.CancellationException) {
                throw cancel
            } catch (err: Throwable) {
                val wait = recallFinishRetryDelayMs(err, repeats, firstSentAtMs, System.currentTimeMillis())
                if (wait == null) {
                    if (recallFinishAnswerLost(err)) throw RecallFinishUnanswered(request, firstSentAtMs, err)
                    throw err
                }
                repeats += 1
                kotlinx.coroutines.delay(wait)
            }
        }
    }

    /** Ask an unanswered `recall/finish` again with the request it was sent
     *  with. `401 recall-refused` when the first never landed or its ten
     *  minutes are over. */
    public suspend fun recallFinishResend(request: RecallFinishRequest): RecallResult = recallFinishOnce(request)

    private suspend fun recallFinishOnce(request: RecallFinishRequest): RecallResult {
        val finish: RecallFinishResponse = post("/account/recall/finish", request)
        val attestation = try {
            base64UrlDecodeNoPad(finish.key_reset_attest_b64)
        } catch (_: IllegalArgumentException) {
            throw AuthHttpError(HTTP_OK, "/account/recall/finish", "malformed key_reset_attest_b64")
        }
        if (attestation.size != KEY_RESET_ATTESTATION_LEN) {
            throw AuthHttpError(
                HTTP_OK,
                "/account/recall/finish",
                "key reset attestation of ${attestation.size} bytes",
            )
        }
        return RecallResult(
            reset = KeyResetResult(
                attestation = attestation,
                keyGeneration = finish.key_generation,
                session = SessionPair(
                    accessToken = finish.session_token,
                    accessExpiresAtMs = finish.session_expires_at_ms,
                    refreshToken = finish.refresh_token,
                    refreshExpiresAtMs = finish.refresh_expires_at_ms,
                    keyGeneration = finish.key_generation,
                ),
            ),
            freshGrant = finish.fresh_grant,
            freshGrantExpiresAtMs = finish.fresh_grant_expires_at_ms,
        )
    }

    // -----------------------------------------------------------------
    // ADR-0066 part 3: rotating the account keys, keeping the devices
    // -----------------------------------------------------------------

    /**
     * `POST /account/keys/rotate/{start,finish}` (bearer): the reset's
     * exchange with a supersession proof by the CURRENT account key over
     * [newSignaturePubkey]. The server verifies the proof against the key it
     * recorded, replaces the recovery credential with one made from [seed]
     * (the NEW 64-byte phrase seed), keeps the passkeys, the password, TOTP
     * and the device floors, deletes the passkey vaults, and gives each id
     * of [revokeDeviceIds] (at most [MAX_ROTATE_REVOKE_IDS]) a floor in the
     * same transaction. The answer is the reset's, without a grant.
     *
     * Nothing changes server-side when `start` fails or `finish` is never
     * sent.
     *
     * @throws AuthHttpError in the server's order: `403 fresh-auth-required`,
     *   `400`, `403 account-key-required`, `429 too-many-attempts`.
     */
    public suspend fun rotateAccountKeys(
        sessionToken: String,
        freshGrant: String,
        seed: ByteArray,
        newSignaturePubkey: ByteArray,
        supersessionProof: ByteArray,
        revokeDeviceIds: List<ByteArray>,
        /** ADR-0067 section 3: this rotation is "Save a recovery phrase". */
        phraseSaved: Boolean = false,
    ): KeyResetResult {
        requireSession(sessionToken)
        if (freshGrant.isEmpty()) throw AuthClientArgError("freshGrant must be non-empty")
        if (seed.isEmpty()) throw AuthClientArgError("seed must be non-empty")
        if (newSignaturePubkey.size != PUBKEY_LEN) {
            throw AuthClientArgError("newSignaturePubkey must be $PUBKEY_LEN bytes (Ed25519)")
        }
        if (supersessionProof.size != SUPERSESSION_PROOF_LEN) {
            throw AuthClientArgError("supersessionProof must be $SUPERSESSION_PROOF_LEN bytes")
        }
        if (revokeDeviceIds.size > MAX_ROTATE_REVOKE_IDS) {
            throw AuthClientArgError("at most $MAX_ROTATE_REVOKE_IDS devices can be revoked with a rotation")
        }
        OpaqueRegister.startRecovery(seed).use { reg ->
            val start: KeyResetStartResponse = post(
                "/account/keys/rotate/start",
                KeyResetStartRequest(fresh_grant = freshGrant, recovery_request = reg.requestB64()),
                bearer = sessionToken,
            )
            val upload = reg.finish(seed, start.recovery_response).use { it.uploadB64() }
            val finish: KeyResetFinishResponse = post(
                "/account/keys/rotate/finish",
                KeyRotateFinishRequest(
                    fresh_grant = freshGrant,
                    recovery_upload = upload,
                    new_signature_pubkey_b64 = base64UrlEncodeNoPad(newSignaturePubkey),
                    supersession_proof_b64 = base64UrlEncodeNoPad(supersessionProof),
                    revoke_device_ids_b64 = revokeDeviceIds.map { base64UrlEncodeNoPad(it) },
                    phrase_saved = if (phraseSaved) true else null,
                ),
                bearer = sessionToken,
            )
            val attestation = try {
                base64UrlDecodeNoPad(finish.key_reset_attest_b64)
            } catch (_: IllegalArgumentException) {
                throw AuthHttpError(HTTP_OK, "/account/keys/rotate/finish", "malformed key_reset_attest_b64")
            }
            if (attestation.size != KEY_RESET_ATTESTATION_LEN) {
                throw AuthHttpError(
                    HTTP_OK,
                    "/account/keys/rotate/finish",
                    "key reset attestation of ${attestation.size} bytes",
                )
            }
            return KeyResetResult(
                attestation = attestation,
                keyGeneration = finish.key_generation,
                session = SessionPair(
                    accessToken = finish.session_token,
                    accessExpiresAtMs = finish.session_expires_at_ms,
                    refreshToken = finish.refresh_token,
                    refreshExpiresAtMs = finish.refresh_expires_at_ms,
                    keyGeneration = finish.key_generation,
                ),
            )
        }
    }

    /**
     * The kept device's session after a rotation (ADR-0066 section 3.3):
     * `POST /auth/session/refresh` with a kind-4 statement by the NEW
     * account key over [refreshToken]. A valid credential of a lower
     * generation is exchanged for a pair at the current one naming the same
     * device. A `statement-stale` answer is signed again at the server's
     * time, once.
     *
     * @throws AuthHttpError `401 key-generation-superseded` (the statement
     *   was not accepted: this device does not hold the account's key),
     *   `401 device-revoked` (this device was the one removed), `401
     *   refresh-invalid`, `429`.
     */
    public suspend fun sessionUpgrade(
        refreshToken: String,
        accountId: ByteArray,
        newAccountEd25519Seed: ByteArray,
    ): SessionPair {
        if (refreshToken.isEmpty()) throw AuthClientArgError("refreshToken must be non-empty")
        val resp: SessionRefreshResponse = sendWithStaleStatementRetry { signAtMs ->
            val statement = if (signAtMs == null) {
                uniffi.lakko_ffi_uniffi.signSessionUpgradeStatement(newAccountEd25519Seed, accountId, refreshToken)
            } else {
                uniffi.lakko_ffi_uniffi.signAccountStatementAt(
                    newAccountEd25519Seed,
                    uniffi.lakko_ffi_uniffi.AccountStatementKind.SESSION_UPGRADE,
                    accountId,
                    uniffi.lakko_ffi_uniffi.sessionUpgradeStatementBody(refreshToken),
                    signAtMs,
                )
            }
            post(
                "/auth/session/refresh",
                SessionRefreshRequest(
                    refresh_token = refreshToken,
                    key_statement_b64 = base64UrlEncodeNoPad(statement),
                ),
            )
        }
        return SessionPair(
            accessToken = resp.session_token,
            accessExpiresAtMs = resp.session_expires_at_ms,
            refreshToken = resp.refresh_token,
            refreshExpiresAtMs = resp.refresh_expires_at_ms,
            keyGeneration = resp.key_generation,
        )
    }

    // -----------------------------------------------------------------
    // ADR-0067: the recovery email
    // -----------------------------------------------------------------

    /** `GET /recovery-email/available` (anonymous): whether this server can
     *  mail a recovery code. */
    public suspend fun recoveryEmailAvailable(): Boolean {
        val res: RecoveryEmailAvailableResponse = doRequest(HttpMethod.Get, "/recovery-email/available") {}
        return res.available
    }

    /** `GET /recovery-email` (bearer): the account's protection. Never the
     *  address. */
    public suspend fun recoveryEmailStatus(sessionToken: String): RecoveryEmailStatus {
        requireSession(sessionToken)
        val res: RecoveryEmailStatusResponse = doRequest(HttpMethod.Get, "/recovery-email") {
            header(HttpHeaders.Authorization, "Bearer $sessionToken")
        }
        return RecoveryEmailStatus(
            available = res.available,
            enabled = res.enabled,
            vault = res.vault,
            phraseSaved = res.phrase_saved,
            totp = res.totp,
        )
    }

    /**
     * `POST /recovery-email/enrol/start` (bearer + fresh-auth grant): mail a
     * code to [address] and get the salt the vault is sealed with.
     *
     * @throws AuthHttpError `403 fresh-auth-required`, `400 address-invalid`,
     *   `409 handle-mismatch` / `username-required`, `429 email-rate-limited`,
     *   `502 mail-send-failed`, `503 recovery-email-disabled`.
     */
    public suspend fun recoveryEmailEnrolStart(
        sessionToken: String,
        freshGrant: String?,
        handle: ByteArray,
        address: String,
        lang: String?,
    ): RecoveryEmailEnrolStart {
        requireSession(sessionToken)
        requireHandle(handle)
        val res: RecoveryEmailEnrolStartResponse = post(
            "/recovery-email/enrol/start",
            RecoveryEmailEnrolStartRequest(
                fresh_grant = freshGrant,
                handle = base64UrlEncodeNoPad(handle),
                address = address,
                lang = lang,
            ),
            bearer = sessionToken,
        )
        val salt = try {
            base64Decode(res.vault_salt)
        } catch (_: IllegalArgumentException) {
            throw AuthHttpError(HTTP_OK, "/recovery-email/enrol/start", "malformed vault_salt")
        }
        if (salt.size != EMAIL_VAULT_SALT_LEN) {
            throw AuthHttpError(HTTP_OK, "/recovery-email/enrol/start", "vault salt of ${salt.size} bytes")
        }
        return RecoveryEmailEnrolStart(
            challengeId = res.challenge_id,
            vaultSalt = salt,
            resendAfterSecs = res.resend_after_secs,
            expiresInSecs = res.expires_in_secs,
        )
    }

    /**
     * `POST /recovery-email/enrol/finish` (bearer): the mailed code, and the
     * sealed vault for an account without a saved phrase. Returns whether a
     * vault was stored.
     *
     * @throws AuthHttpError `401 code-refused`, `410 code-expired`, `409
     *   phrase-saved`, `429 too-many-attempts`.
     */
    public suspend fun recoveryEmailEnrolFinish(
        sessionToken: String,
        challengeId: String,
        code: String,
        sealedEntropy: ByteArray?,
    ): Boolean {
        requireSession(sessionToken)
        if (sealedEntropy != null && sealedEntropy.size != EMAIL_VAULT_SEALED_LEN) {
            throw AuthClientArgError("the email vault must be $EMAIL_VAULT_SEALED_LEN bytes")
        }
        val res: RecoveryEmailEnrolFinishResponse = post(
            "/recovery-email/enrol/finish",
            RecoveryEmailEnrolFinishRequest(
                challenge_id = challengeId,
                code = code,
                sealed_entropy = sealedEntropy?.let { base64Encode(it) },
            ),
            bearer = sessionToken,
        )
        return res.vault
    }

    /** `DELETE /recovery-email` (bearer + fresh-auth grant): remove the
     *  recovery email and the vault. */
    public suspend fun recoveryEmailRemove(sessionToken: String, freshGrant: String?) {
        requireSession(sessionToken)
        sendJson<RecoveryEmailGrantRequest, RecoveryEmailRemoveResponse>(
            HttpMethod.Delete,
            "/recovery-email",
            RecoveryEmailGrantRequest(fresh_grant = freshGrant),
            bearer = sessionToken,
        )
    }

    /** `GET /account/notices` (bearer): one page, oldest first, past the
     *  cursor ([after]) or, for a device with no cursor, since [sinceMs]. */
    public suspend fun accountNotices(
        sessionToken: String,
        after: Long?,
        sinceMs: Long? = null,
    ): AccountNoticesResponse {
        requireSession(sessionToken)
        val query = when {
            after != null -> "?after=$after"
            sinceMs != null -> "?since_ms=$sinceMs"
            else -> ""
        }
        return doRequest(HttpMethod.Get, "/account/notices$query") {
            header(HttpHeaders.Authorization, "Bearer $sessionToken")
        }
    }

    /**
     * `POST /recovery-email/recover/start` (anonymous): the same answer for a
     * matching pair, a wrong pair, an unknown name and an account without an
     * email.
     */
    public suspend fun emailRecoverStart(handle: ByteArray, address: String, lang: String?): EmailRecoverStartResponse {
        requireHandle(handle)
        return post(
            "/recovery-email/recover/start",
            EmailRecoverStartRequest(handle = base64UrlEncodeNoPad(handle), address = address, lang = lang),
        )
    }

    /** `POST /recovery-email/recover/verify`: `401 code-refused` for a wrong
     *  code and for a code that was never sent. */
    public suspend fun emailRecoverVerify(challengeId: String, code: String): EmailRecoverVerifyResponse =
        post("/recovery-email/recover/verify", EmailRecoverVerifyRequest(challenge_id = challengeId, code = code))

    /** `POST /recovery-email/recover/totp`: the authenticator's code. */
    public suspend fun emailRecoverTotp(ticket: String, code: String) {
        val res: EmailRecoverOkResponse =
            post("/recovery-email/recover/totp", EmailRecoverTotpRequest(ticket = ticket, code = code))
        if (!res.ok) throw AuthHttpError(HTTP_OK, "/recovery-email/recover/totp", "server reported ok=false")
    }

    /** `POST /recovery-email/recover/open`: the sealed keys (the ticket is
     *  then spent) or the password-only mode. */
    public suspend fun emailRecoverOpen(ticket: String): EmailRecoverOpen {
        val endpoint = "/recovery-email/recover/open"
        val res: EmailRecoverOpenResponse = post(endpoint, EmailRecoverTicketRequest(ticket = ticket))
        return when (res.mode) {
            "password" -> EmailRecoverOpen.Password
            "vault" -> {
                val salt = res.vault_salt?.let(::decodeOrNull)
                val sealed = res.sealed_entropy?.let(::decodeOrNull)
                if (salt == null || sealed == null ||
                    salt.size != EMAIL_VAULT_SALT_LEN || sealed.size != EMAIL_VAULT_SEALED_LEN
                ) {
                    throw AuthHttpError(HTTP_OK, endpoint, "malformed vault")
                }
                EmailRecoverOpen.Vault(vaultSalt = salt, sealedEntropy = sealed)
            }
            else -> throw AuthHttpError(HTTP_OK, endpoint, "unknown mode")
        }
    }

    private fun recallStatusOf(res: RecallStatusResponse, byHandle: Boolean): RecallStatus = RecallStatus(
        anchorSeq = res.anchor_seq,
        expiresAtMs = res.expires_at_ms,
        changedAtMs = res.changed_at_ms,
        attemptsRemaining = res.attempts_remaining,
        // Only a device that asked by handle needs to be told the account.
        accountId = res.account_id_b64?.takeIf { byHandle }?.let {
            try {
                base64UrlDecodeNoPad(it).takeIf { id -> id.size == ACCOUNT_ID_LEN }
            } catch (_: IllegalArgumentException) {
                null
            }
        },
        phraseArm = res.phrase_arm,
    )

    private fun decodeOrNull(b64: String): ByteArray? = try {
        base64Decode(b64)
    } catch (_: IllegalArgumentException) {
        null
    }

    /**
     * `POST /recovery-email/recover/password/{start,finish}`: choose a new
     * password under the ticket (an account with a saved phrase). The answer
     * is the session pair and the account id; no fresh-auth grant.
     */
    public suspend fun emailRecoverSetPassword(
        ticket: String,
        newPassword: ByteArray,
        deviceId: ByteArray? = null,
    ): TotpRecoverResult {
        if (newPassword.isEmpty()) throw AuthClientArgError("newPassword must be non-empty")
        OpaqueRegister.startPassword(newPassword).use { reg ->
            val start: EmailRecoverPasswordStartResponse = post(
                "/recovery-email/recover/password/start",
                EmailRecoverPasswordStartRequest(ticket = ticket, new_password_request = reg.requestB64()),
            )
            val upload = reg.finish(newPassword, start.new_password_response).use { it.uploadB64() }
            val finish: EmailRecoverPasswordFinishResponse = post(
                "/recovery-email/recover/password/finish",
                EmailRecoverPasswordFinishRequest(
                    ticket = ticket,
                    new_password_upload = upload,
                    device_id_b64 = deviceIdField(deviceId),
                ),
            )
            val accountId = try {
                base64UrlDecodeNoPad(finish.account_id_b64)
            } catch (_: IllegalArgumentException) {
                throw AuthHttpError(HTTP_OK, "/recovery-email/recover/password/finish", "malformed account_id_b64")
            }
            return TotpRecoverResult(
                accountId = accountIdFromFinish(accountId, "/recovery-email/recover/password/finish"),
                session = SessionPair(
                    accessToken = finish.session_token,
                    accessExpiresAtMs = finish.session_expires_at_ms,
                    refreshToken = finish.refresh_token,
                    refreshExpiresAtMs = finish.refresh_expires_at_ms,
                    keyGeneration = finish.key_generation,
                ),
            )
        }
    }

    // -----------------------------------------------------------------
    // Passkey vault: the keys a passkey restores (opt-in per passkey)
    // -----------------------------------------------------------------

    /** `PUT /vault/passkey`: store the 44-byte sealed phrase entropy for one
     *  of the account's passkeys. `503` when the server keeps no such keys. */
    public suspend fun passkeyVaultPut(
        sessionToken: String,
        credentialId: ByteArray,
        sealedEntropy: ByteArray,
    ) {
        requireSession(sessionToken)
        sendJson<PasskeyVaultPutRequest, PasskeyVaultPutResponse>(
            HttpMethod.Put,
            "/vault/passkey",
            PasskeyVaultPutRequest(
                credential_id_b64 = base64UrlEncodeNoPad(credentialId),
                sealed_entropy_b64 = base64Encode(sealedEntropy),
            ),
            bearer = sessionToken,
        )
    }

    /** `POST /vault/passkey/get`: the sealed blob stored for [credentialId],
     *  or `null` when none is. */
    public suspend fun passkeyVaultGet(sessionToken: String, credentialId: ByteArray): ByteArray? {
        requireSession(sessionToken)
        val resp: PasskeyVaultGetResponse = post(
            "/vault/passkey/get",
            PasskeyVaultCredentialRequest(credential_id_b64 = base64UrlEncodeNoPad(credentialId)),
            bearer = sessionToken,
        )
        return resp.sealed_entropy_b64?.let { base64Decode(it) }
    }

    /** `DELETE /vault/passkey`: drop the blob stored for a passkey ("Stop
     *  restoring keys"). [credentialIdB64] is base64url without padding, as
     *  the credential list gives it. Returns whether a blob was deleted. */
    public suspend fun passkeyVaultDelete(sessionToken: String, credentialIdB64: String): Boolean {
        requireSession(sessionToken)
        val resp: PasskeyVaultDeleteResponse = sendJson(
            HttpMethod.Delete,
            "/vault/passkey",
            PasskeyVaultCredentialRequest(credential_id_b64 = credentialIdB64),
            bearer = sessionToken,
        )
        return resp.deleted
    }

    /** `GET /vault/passkeys`: the credential ids (base64url, no padding) of
     *  the account's passkeys that restore keys. */
    public suspend fun passkeyVaultList(sessionToken: String): List<String> {
        requireSession(sessionToken)
        val resp: PasskeyVaultListResponse = doRequest(HttpMethod.Get, "/vault/passkeys") {
            header(HttpHeaders.Authorization, "Bearer $sessionToken")
        }
        return resp.credential_ids_b64
    }

    private fun requireSession(sessionToken: String) {
        if (sessionToken.isEmpty()) {
            throw AuthClientArgError("sessionToken must be non-empty")
        }
    }

    // -----------------------------------------------------------------
    // Device link - ADR-0049 section 2 (amended 2026-09-28)
    // -----------------------------------------------------------------

    /** `POST /device/request` (anonymous): open a request for the account
     *  [handle] names; the raw 16-byte request id. */
    public suspend fun deviceRequestCreate(handle: ByteArray): ByteArray {
        requireHandle(handle)
        val resp: DeviceRequestCreateResponse =
            post("/device/request", DeviceRequestCreateBody(handle = base64UrlEncodeNoPad(handle)))
        return base64UrlDecodeNoPad(resp.request_id_b64)
    }

    /** `POST /device/request/status` (anonymous). */
    public suspend fun deviceRequestStatus(requestId: ByteArray): me.lakko.app.devices.DeviceRequestStatus {
        val resp: DeviceRequestStatusResponse =
            post("/device/request/status", DeviceRequestStatusBody(request_id_b64 = base64UrlEncodeNoPad(requestId)))
        return me.lakko.app.devices.DeviceRequestStatus(
            pending = resp.pending,
            pakeB = resp.pake_b_b64?.let(::base64Decode),
            outcome = resp.outcome,
            confirmB = resp.confirm_b_b64?.let(::base64Decode),
        )
    }

    /** `POST /device/request/payload` (anonymous), once per request. */
    public suspend fun deviceRequestPayload(
        requestId: ByteArray,
        pakeA: ByteArray,
        sealedPayload: ByteArray,
        confirmA: ByteArray,
    ) {
        post<DeviceRequestPayloadBody, kotlinx.serialization.json.JsonObject>(
            "/device/request/payload",
            DeviceRequestPayloadBody(
                request_id_b64 = base64UrlEncodeNoPad(requestId),
                pake_a_b64 = base64Encode(pakeA),
                sealed_payload_b64 = base64Encode(sealedPayload),
                confirm_a_b64 = base64Encode(confirmA),
            ),
        )
    }

    /** `GET /device/requests` (bearer): the account's open request, or `null`. */
    public suspend fun deviceRequestsList(sessionToken: String): me.lakko.app.devices.PendingDeviceRequest? {
        val resp: DeviceRequestListResponse = doRequest(HttpMethod.Get, "/device/requests") {
            header(HttpHeaders.Authorization, "Bearer $sessionToken")
        }
        val r = resp.request ?: return null
        return me.lakko.app.devices.PendingDeviceRequest(
            requestId = base64UrlDecodeNoPad(r.request_id_b64),
            pakeB = r.pake_b_b64?.let(::base64Decode),
            pakeA = r.pake_a_b64?.let(::base64Decode),
            sealedPayload = r.sealed_payload_b64?.let(::base64Decode),
            confirmA = r.confirm_a_b64?.let(::base64Decode),
            outcome = r.outcome,
        )
    }

    /** `POST /device/requests/{id}/pake` (bearer): the answer, once. */
    public suspend fun deviceRequestAnswer(
        sessionToken: String,
        requestId: ByteArray,
        pakeB: ByteArray,
        freshGrant: String,
    ): me.lakko.app.devices.DeviceLinkAnswer =
        try {
            sendJson<DeviceRequestAnswerBody, kotlinx.serialization.json.JsonObject>(
                HttpMethod.Post,
                "/device/requests/${base64UrlEncodeNoPad(requestId)}/pake",
                DeviceRequestAnswerBody(pake_b_b64 = base64Encode(pakeB), fresh_grant = freshGrant),
                bearer = sessionToken,
            )
            me.lakko.app.devices.DeviceLinkAnswer.ANSWERED
        } catch (e: AuthHttpError) {
            when (e.status) {
                HTTP_CONFLICT -> me.lakko.app.devices.DeviceLinkAnswer.ALREADY_ANSWERED
                HTTP_NOT_FOUND -> me.lakko.app.devices.DeviceLinkAnswer.NOT_FOUND
                else -> throw e
            }
        }

    /** `POST /device/requests/{id}/result` (bearer), once. */
    public suspend fun deviceRequestResult(
        sessionToken: String,
        requestId: ByteArray,
        accepted: Boolean,
        confirmB: ByteArray?,
        freshGrant: String,
    ) {
        sendJson<DeviceRequestResultBody, kotlinx.serialization.json.JsonObject>(
            HttpMethod.Post,
            "/device/requests/${base64UrlEncodeNoPad(requestId)}/result",
            DeviceRequestResultBody(
                outcome = if (accepted) me.lakko.app.devices.OUTCOME_ACCEPTED else me.lakko.app.devices.OUTCOME_WRONG_CODE,
                confirm_b_b64 = if (accepted) confirmB?.let(::base64Encode) else null,
                fresh_grant = freshGrant,
            ),
            bearer = sessionToken,
        )
    }

    /** `DELETE /device/requests/{id}` (bearer): "It is not me". */
    public suspend fun deviceRequestDecline(sessionToken: String, requestId: ByteArray) {
        doRequest<kotlinx.serialization.json.JsonObject>(
            HttpMethod.Delete,
            "/device/requests/${base64UrlEncodeNoPad(requestId)}",
        ) {
            header(HttpHeaders.Authorization, "Bearer $sessionToken")
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

        /** ADR-0066 - a device id is its 32-byte leaf signature key. */
        public const val DEVICE_ID_LEN: Int = 32

        /** Ed25519 identity pubkey length (ADR-0036 genesis-attest mint). */
        private const val PUBKEY_LEN: Int = 32

        /** The fixed layout of the ADR-0065 key-reset attestation. */
        public const val KEY_RESET_ATTESTATION_LEN: Int = 177
        /** A supersession proof is this many bytes (ADR-0016). */
        public const val SUPERSESSION_PROOF_LEN: Int = 145
        /** `rotate/finish` revokes at most this many devices. */
        public const val MAX_ROTATE_REVOKE_IDS: Int = 8

        /** A 2xx whose body failed a shape check. */
        private const val HTTP_OK: Int = 200

        private const val HTTP_UNAUTHORIZED: Int = 401

        /** `/signup/finish` without a live start (ADR-0054 S4). */
        private const val HTTP_GONE: Int = 410

        /** `PUT /auth/login-handle` for a handle another account holds. */
        private const val HTTP_CONFLICT: Int = 409

        private const val HTTP_NOT_FOUND: Int = 404

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
    /** The 12-word recovery phrase, present when signup was called
     *  with `enrolRecovery = true` (the default). The UI MUST
     *  display it exactly once and ask the user to record it
     *  somewhere safe — losing it means losing the only way back
     *  into the account if the password is forgotten. */
    val recoveryPhrase: String? = null,
    /** ADR-0065: the session pair (access token, refresh credential, key
     *  generation). Stash in [AuthRepository.identitySecrets]. */
    val session: SessionPair,
    /** The fresh-auth proof the sign-up minted (UX section 17), opaque;
     *  `null` when the server sent none. Memory only. */
    val freshGrant: String? = null,
    /** The proof's expiry, unix milliseconds. */
    val freshGrantExpiresAtMs: Long? = null,
) {
    /** Never print the credentials, the phrase or the grant. */
    override fun toString(): String = "SignupResult(<redacted>)"

    /** The access token of [session]. */
    val sessionToken: String get() = session.accessToken

    /** The access token's expiry as a unix-millisecond timestamp. */
    val sessionExpiresAtMs: Long get() = session.accessExpiresAtMs

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
            session == other.session
    }

    override fun hashCode(): Int {
        var result = accountId.contentHashCode()
        result = 31 * result + exportKey.contentHashCode()
        result = 31 * result + (recoveryPhrase?.hashCode() ?: 0)
        result = 31 * result + session.hashCode()
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
    /** ADR-0065: the session pair. */
    val session: SessionPair,
    /** The fresh-auth proof the password login minted (UX section 17),
     *  opaque; `null` when the server sent none. Memory only: never stored,
     *  never logged. */
    val freshGrant: String? = null,
    /** The proof's expiry, unix milliseconds. */
    val freshGrantExpiresAtMs: Long? = null,
) {
    val sessionToken: String get() = session.accessToken

    val sessionExpiresAtMs: Long get() = session.accessExpiresAtMs

    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is LoginResult) return false
        return accountId.contentEquals(other.accountId) &&
            sessionKey.contentEquals(other.sessionKey) &&
            exportKey.contentEquals(other.exportKey) &&
            session == other.session &&
            freshGrant == other.freshGrant &&
            freshGrantExpiresAtMs == other.freshGrantExpiresAtMs
    }

    /** Never print the credentials or the grant. */
    override fun toString(): String = "LoginResult(<redacted>)"

    override fun hashCode(): Int {
        var result = accountId.contentHashCode()
        result = 31 * result + sessionKey.contentHashCode()
        result = 31 * result + exportKey.contentHashCode()
        result = 31 * result + session.hashCode()
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
    /** ADR-0065: the session pair for the rotated password. */
    val session: SessionPair,
) {
    val sessionToken: String get() = session.accessToken

    val sessionExpiresAtMs: Long get() = session.accessExpiresAtMs

    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is RecoverResult) return false
        return accountId.contentEquals(other.accountId) &&
            exportKey.contentEquals(other.exportKey) &&
            session == other.session
    }

    override fun hashCode(): Int {
        var result = accountId.contentHashCode()
        result = 31 * result + exportKey.contentHashCode()
        result = 31 * result + session.hashCode()
        return result
    }
}

/** Result of [AuthClient.deviceAuthorizeRedeem] — the same
 *  `(account_id, session pair)` every other login flow returns (ADR-0018
 *  factor (a), fresh-device side). No exportKey: this path has no
 *  password, so no OPAQUE export key exists on this device until the user
 *  later sets one. */
public data class DeviceAuthorizeRedeemResult(
    val accountId: ByteArray,
    val session: SessionPair,
) {
    val sessionToken: String get() = session.accessToken

    val sessionExpiresAtMs: Long get() = session.accessExpiresAtMs

    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is DeviceAuthorizeRedeemResult) return false
        return accountId.contentEquals(other.accountId) && session == other.session
    }

    override fun hashCode(): Int {
        var result = accountId.contentHashCode()
        result = 31 * result + session.hashCode()
        return result
    }
}

/**
 * Result of [AuthClient.resetAccountKeys] (ADR-0065): the attestation
 * discovery and the Delivery Service require before they let the new
 * identity replace the old one, and the pair this device continues with.
 */
public class KeyResetResult(
    /** The raw 177-byte `KeyResetAttestation`, verified by the caller
     *  against the pinned attest key before any use. */
    public val attestation: ByteArray,
    /** The generation the reset moved the account to. */
    public val keyGeneration: Long,
    /** The pair issued at [keyGeneration]; the caller's old one is gone. */
    public val session: SessionPair,
)

/** Result of [AuthClient.recallAccount]: the key reset's result and the
 *  fresh-auth proof the exchange earned. */
public class RecallResult(
    public val reset: KeyResetResult,
    public val freshGrant: String?,
    public val freshGrantExpiresAtMs: Long?,
) {
    override fun toString(): String = "RecallResult(<redacted>)"
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

/** Result of [AuthClient.totpRecover] and [AuthClient.recoveryLogin] — the
 *  same `(account_id, session pair)` every other login flow returns
 *  (ADR-0018 factor (c)). No exportKey: these routes involve no password,
 *  so no OPAQUE export key exists on this device. */
public data class TotpRecoverResult(
    val accountId: ByteArray,
    val session: SessionPair,
) {
    val sessionToken: String get() = session.accessToken

    val sessionExpiresAtMs: Long get() = session.accessExpiresAtMs

    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is TotpRecoverResult) return false
        return accountId.contentEquals(other.accountId) && session == other.session
    }

    override fun hashCode(): Int {
        var result = accountId.contentHashCode()
        result = 31 * result + session.hashCode()
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
public class AuthClientArgError(
    message: String,
    /** The sentence a screen shows for this refusal (ADR-0061 section 6);
     *  `null` for a caller-contract diagnostic, which a screen replaces with
     *  its own generic sentence. [message] stays English for logs. */
    public val text: me.lakko.app.l10n.UiText? = null,
) : IllegalArgumentException(message)

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
