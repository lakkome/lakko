// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import io.ktor.client.HttpClient
import io.ktor.client.call.body
import io.ktor.client.engine.HttpClientEngine
import io.ktor.client.engine.okhttp.OkHttp
import io.ktor.client.plugins.contentnegotiation.ContentNegotiation
import io.ktor.client.request.HttpRequestBuilder
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.bodyAsText
import io.ktor.http.ContentType
import io.ktor.http.contentType
import io.ktor.http.isSuccess
import io.ktor.serialization.kotlinx.json.json
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.serialization.json.Json
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
 *      `/pow/challenge` → `/signup/start` → `/pow/challenge` →
 *      `/signup/finish` (and the equivalent for login and
 *      recover). They drive the UniFFI OPAQUE handles
 *      ([OpaqueRegister] / [OpaqueLogin]), the BIP39
 *      [RecoveryMnemonic], and the [solvePow] helper.
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
 *   diagnostics.
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
    /** Dispatcher for the PoW solver. Defaults to [Dispatchers.Default];
     *  override to `UnconfinedTestDispatcher` in `runTest` blocks. */
    private val powDispatcher: CoroutineDispatcher = Dispatchers.Default,
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
        HttpClient(OkHttp) {
            install(ContentNegotiation) { json(json) }
        }
    }

    override fun close() {
        client.close()
    }

    // -----------------------------------------------------------------
    // Low-level: 1:1 HTTP endpoints
    // -----------------------------------------------------------------

    /** `POST /pow/challenge`. The only endpoint that takes no body. */
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
    // High-level: full signup / login / recover flows
    // -----------------------------------------------------------------

    /**
     * Drive the full signup flow:
     *
     * ```text
     *   POST /pow/challenge   →  seed₁
     *   client: solve PoW + OPAQUE password start [+ recovery start]
     *   POST /signup/start    →  password_response [+ recovery_response]
     *   client: OPAQUE finish (both flows)
     *   POST /pow/challenge   →  seed₂
     *   client: solve PoW
     *   POST /signup/finish   →  ok
     * ```
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
    ): SignupResult {
        if (password.isEmpty()) {
            throw AuthClientArgError("password must be non-empty")
        }

        val accountId = generateAccountId()
        val accountIdB64 = base64Encode(accountId)

        // ------------- start
        val challenge1 = powChallenge()
        val pow1 = solvePow(challenge1.seed, challenge1.effort_bits, powDispatcher)

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
                        pow = pow1,
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

                    val challenge2 = powChallenge()
                    val pow2 = solvePow(challenge2.seed, challenge2.effort_bits, powDispatcher)

                    val finishResp = signupFinish(
                        SignupFinishRequest(
                            pow = pow2,
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
     *   POST /pow/challenge  →  seed
     *   client: solve PoW + OPAQUE start
     *   POST /login/start    →  session_id, response
     *   client: OPAQUE finish
     *   POST /login/finish   →  ok, account_id
     * ```
     *
     * [accountId] MUST be the raw 16-byte id the server gave back
     * at signup time. The Compose side will pin it in
     * `EncryptedSharedPreferences` keyed by username (1.15d3).
     *
     * Wrong passwords surface as an [AuthHttpError] with status
     * 401 — by design, indistinguishable from "unknown account".
     */
    public suspend fun login(
        accountId: ByteArray,
        password: ByteArray,
    ): LoginResult {
        if (password.isEmpty()) {
            throw AuthClientArgError("password must be non-empty")
        }
        if (accountId.size != ACCOUNT_ID_LEN) {
            throw AuthClientArgError("accountId must be $ACCOUNT_ID_LEN bytes")
        }

        val accountIdB64 = base64Encode(accountId)
        val challenge = powChallenge()
        val pow = solvePow(challenge.seed, challenge.effort_bits, powDispatcher)

        OpaqueLogin.startPassword(password).use { login ->
            val startResp = loginStart(
                LoginStartRequest(
                    pow = pow,
                    account_id = accountIdB64,
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
                    accountId = accountId,
                    sessionKey = out.sessionKey(),
                    exportKey = out.exportKey(),
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
     *   POST /pow/challenge          →  seed
     *   client: solve PoW
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
     * Note: the recovery flow only requires **one** PoW solve
     * (gating `/start`), unlike signup which needs two.
     * Wrong-mnemonic attempts fail at the AKE finalization with
     * the same indistinguishable response as `/recovery/login/finish`.
     *
     * @param accountId    Raw 16-byte UUIDv7. The recovery flow can
     *                     only target an account whose id is known
     *                     locally; until 1.16d's discovery service
     *                     lands, that means the account that signed
     *                     up on this device.
     * @param mnemonicSeed The 64-byte BIP39 seed derived from the
     *                     user's typed phrase via
     *                     `RecoveryMnemonic.fromPhrase(phrase).toSeed()`.
     *                     Caller is responsible for surfacing
     *                     mnemonic-parsing errors before reaching
     *                     this method.
     * @param newPassword  The replacement password as raw UTF-8 bytes.
     */
    public suspend fun recover(
        accountId: ByteArray,
        mnemonicSeed: ByteArray,
        newPassword: ByteArray,
    ): RecoverResult {
        if (accountId.size != ACCOUNT_ID_LEN) {
            throw AuthClientArgError("accountId must be $ACCOUNT_ID_LEN bytes")
        }
        if (mnemonicSeed.isEmpty()) {
            throw AuthClientArgError("mnemonicSeed must be non-empty")
        }
        if (newPassword.isEmpty()) {
            throw AuthClientArgError("newPassword must be non-empty")
        }

        val accountIdB64 = base64Encode(accountId)

        // Single PoW solve — the server only gates /start.
        val challenge = powChallenge()
        val pow = solvePow(challenge.seed, challenge.effort_bits, powDispatcher)

        // Two parallel OPAQUE flows: recovery login proves the user
        // knows the mnemonic; password registration enrols the new
        // password envelope. Both are independent — neither needs
        // the other's intermediate state.
        OpaqueLogin.startRecovery(mnemonicSeed).use { recoveryLogin ->
            OpaqueRegister.startPassword(newPassword).use { newPasswordReg ->
                val startResp = passwordResetStart(
                    PasswordResetStartRequest(
                        pow = pow,
                        account_id = accountIdB64,
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
                            accountId = accountId,
                            exportKey = passwordOut.exportKey(),
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
    ): Res = doPost(endpoint) {
        contentType(ContentType.Application.Json)
        setBody(body)
    }

    /**
     * One-shot POST with no body. Used only for `/pow/challenge`,
     * the single endpoint the server accepts without an
     * application/json body.
     */
    private suspend inline fun <reified Res : Any> postNoBody(
        endpoint: String,
    ): Res = doPost(endpoint) { /* no body */ }

    /**
     * Shared HTTP plumbing — opens the connection, maps transport
     * errors to [AuthNetworkError], maps non-2xx responses to
     * [AuthHttpError] with the truncated server body, and
     * deserializes the response body.
     *
     * Inlined so [Res] stays reified through to `response.body()`,
     * which is the only place Ktor needs the type witness.
     */
    private suspend inline fun <reified Res : Any> doPost(
        endpoint: String,
        crossinline configure: HttpRequestBuilder.() -> Unit,
    ): Res {
        val url = "$baseUrl$endpoint"
        val response: HttpResponse = try {
            client.post(url) { configure() }
        } catch (cause: Throwable) {
            throw AuthNetworkError(endpoint, cause)
        }

        if (!response.status.isSuccess()) {
            val text = response.safeReadBody()
            throw AuthHttpError(response.status.value, endpoint, text)
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
         * `10.0.2.2` is the canonical "host machine from inside the
         * Android emulator" loopback alias — every Google AVD maps
         * it to the workstation running adb. Port 8081 matches the
         * `LAKKO_BIND` dev setup in [CLAUDE.md][1] for
         * `lakko-svc-auth`.
         *
         * On a physical device on the same Wi-Fi as the dev
         * workstation, pass the workstation's LAN IP via the
         * [AuthClient] constructor instead. Production deployments
         * use the `https://api-dev.lakko.me` Cloudflare tunnel
         * URL via the same constructor parameter.
         *
         * [1]: ../../../../../../../../../CLAUDE.md
         */
        public const val DEFAULT_BASE_URL: String = "http://10.0.2.2:8081"

        private const val ACCOUNT_ID_LEN: Int = 16
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
            recoveryPhrase == other.recoveryPhrase
    }

    override fun hashCode(): Int {
        var result = accountId.contentHashCode()
        result = 31 * result + exportKey.contentHashCode()
        result = 31 * result + (recoveryPhrase?.hashCode() ?: 0)
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
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is LoginResult) return false
        return accountId.contentEquals(other.accountId) &&
            sessionKey.contentEquals(other.sessionKey) &&
            exportKey.contentEquals(other.exportKey)
    }

    override fun hashCode(): Int {
        var result = accountId.contentHashCode()
        result = 31 * result + sessionKey.contentHashCode()
        result = 31 * result + exportKey.contentHashCode()
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
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is RecoverResult) return false
        return accountId.contentEquals(other.accountId) &&
            exportKey.contentEquals(other.exportKey)
    }

    override fun hashCode(): Int {
        var result = accountId.contentHashCode()
        result = 31 * result + exportKey.contentHashCode()
        return result
    }
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
) : RuntimeException("auth $endpoint → $status: $body")

/** Network failure (DNS / connectivity / TLS). Distinct from
 *  [AuthHttpError] so the UI can suggest "check connection" rather
 *  than "wrong credentials". */
public class AuthNetworkError(
    public val endpoint: String,
    cause: Throwable,
) : RuntimeException("auth $endpoint network error: $cause", cause)
