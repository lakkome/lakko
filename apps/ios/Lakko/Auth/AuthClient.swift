// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// URLSession-based client for `lakko-svc-auth`.
///
/// Mirror of:
///
///   * `apps/web/src/auth/client.ts` (1.17d4)
///   * `apps/android/.../auth/AuthClient.kt` (1.15d2)
///
/// Two layers of API:
///
///   1. **Low-level methods** mirror the seven HTTP endpoints 1:1.
///      Take a typed request, return a typed response, do nothing
///      else. Useful for tests, for the recovery flow, and for the
///      SwiftUI screens that 1.14d3–5 build on top.
///
///   2. **High-level ``signup(password:enrolRecovery:)`` /
///      ``login(accountId:password:)`` /
///      ``recover(accountId:mnemonicSeed:newPassword:)``** orchestrate
///      the full multi-round-trip flow:
///      `/pow/challenge` → `/signup/start` → `/pow/challenge` →
///      `/signup/finish` (and the equivalent for login and recover).
///      They drive the UniFFI OPAQUE handles (``OpaqueRegister`` /
///      ``OpaqueLogin``), the BIP39 ``RecoveryMnemonic``, and the
///      ``solvePow(seedB64:effortBits:)`` helper.
///
/// ## Lifecycle
///
/// One ``AuthClient`` per app process is the recommended pattern.
/// `URLSession` (the default `.shared` or a custom instance) is
/// thread-safe and reuses its underlying NSURLProtocol stack across
/// calls; construct ``AuthClient`` once at app start and reuse.
/// Unlike Ktor's heavyweight `HttpClient`, there's nothing to
/// `close` — `URLSession` cleans up via ARC when the last reference
/// goes away.
///
/// ## Default base URL
///
/// `http://localhost:8081` — the canonical "host machine from inside
/// the iOS simulator" address (the simulator shares the host's
/// network namespace, unlike the Android emulator which needs
/// `10.0.2.2`). On a physical device pointed at a dev workstation,
/// override via the constructor with the workstation's LAN IP.
/// Production switches to `https://api-dev.lakko.me` via the
/// Cloudflare tunnel.
///
/// ## Errors
///
/// * ``AuthClientError/argument(_:)`` — caller-side wiring bug
///   (empty password, wrong-length account-id). Surface as a form
///   validation message.
/// * ``AuthClientError/http(status:endpoint:body:)`` — server
///   returned a non-2xx, or `ok=false` on a `/finish` endpoint.
///   Status + truncated body are exposed for diagnostics.
/// * ``AuthClientError/network(endpoint:underlying:)`` — transport
///   failure (DNS, TLS, connection refused). Distinct from HTTP
///   errors so the UI can suggest "check connection" rather than
///   "wrong credentials".
public final class AuthClient {

    // MARK: - Configuration

    /// Default base URL — the iOS simulator shares the host's
    /// loopback, so `localhost:8081` reaches the `lakko-svc-auth` dev
    /// server. Port matches `LAKKO_BIND` in `CLAUDE.md`.
    public static let defaultBaseURL: String = "http://localhost:8081"

    /// Base URL. No trailing slash.
    public let baseURL: String

    /// Underlying URLSession. Injectable so tests can drop in a
    /// session configured with `URLProtocol`-stubbed responses
    /// (`URLSessionConfiguration.ephemeral` with a custom protocol
    /// class — the Foundation analogue of Ktor's `MockEngine`).
    private let session: URLSession

    /// Shared JSON codec. `JSONEncoder` omits `nil` properties by
    /// default, matching the kotlinx-serialization
    /// `explicitNulls = false` and the TS spread-operator shape on
    /// the wire. `JSONDecoder` is lenient on unknown keys by default
    /// — Foundation simply ignores them, keeping us
    /// forward-compatible with future server fields we don't know
    /// about yet (the server promise is to only **add** fields, the
    /// wire format is append-only).
    private let encoder: JSONEncoder
    private let decoder: JSONDecoder

    public init(
        baseURL: String = AuthClient.defaultBaseURL,
        session: URLSession = .shared
    ) {
        self.baseURL = baseURL
        self.session = session
        self.encoder = JSONEncoder()
        self.decoder = JSONDecoder()
    }

    // MARK: - Low-level: 1:1 HTTP endpoints

    /// `POST /pow/challenge`. The only endpoint that takes no body.
    public func powChallenge() async throws -> PowChallengeResponse {
        try await postNoBody(endpoint: "/pow/challenge")
    }

    /// `POST /signup/start`.
    public func signupStart(
        _ request: SignupStartRequest
    ) async throws -> SignupStartResponse {
        try await post(endpoint: "/signup/start", body: request)
    }

    /// `POST /signup/finish`.
    public func signupFinish(
        _ request: SignupFinishRequest
    ) async throws -> SignupFinishResponse {
        try await post(endpoint: "/signup/finish", body: request)
    }

    /// `POST /login/start`.
    public func loginStart(
        _ request: LoginStartRequest
    ) async throws -> LoginStartResponse {
        try await post(endpoint: "/login/start", body: request)
    }

    /// `POST /login/finish`.
    public func loginFinish(
        _ request: LoginFinishRequest
    ) async throws -> LoginFinishResponse {
        try await post(endpoint: "/login/finish", body: request)
    }

    /// `POST /recovery/login/start`.
    public func recoveryLoginStart(
        _ request: LoginStartRequest
    ) async throws -> LoginStartResponse {
        try await post(endpoint: "/recovery/login/start", body: request)
    }

    /// `POST /recovery/login/finish`.
    public func recoveryLoginFinish(
        _ request: LoginFinishRequest
    ) async throws -> LoginFinishResponse {
        try await post(endpoint: "/recovery/login/finish", body: request)
    }

    /// `POST /password/reset/start`.
    public func passwordResetStart(
        _ request: PasswordResetStartRequest
    ) async throws -> PasswordResetStartResponse {
        try await post(endpoint: "/password/reset/start", body: request)
    }

    /// `POST /password/reset/finish`.
    public func passwordResetFinish(
        _ request: PasswordResetFinishRequest
    ) async throws -> PasswordResetFinishResponse {
        try await post(endpoint: "/password/reset/finish", body: request)
    }

    // MARK: - Device authorization — ADR-0018 factor (a),
    // "another-device" recovery

    /// `POST /device/authorize/start` — mint a single-use 32-byte
    /// device-authorization grant for this account (ADR-0018 factor
    /// (a), scanner side). Mirror of the web
    /// `AuthClient.deviceAuthorizeStart` + the Android
    /// `AuthClient.deviceAuthorizeStart`.
    ///
    /// Bearer-authenticated (the EXISTING device proves it already
    /// holds a session) + PoW-gated. Returns the URL-safe base64
    /// (no pad) of the RAW grant token — the server stores only its
    /// SHA-256. The caller seals it (together with the phrase seed)
    /// to the fresh device over the E2EE pairing channel; it never
    /// travels in plaintext past this response.
    public func deviceAuthorizeStart(sessionToken: String) async throws -> String {
        guard !sessionToken.isEmpty else {
            throw AuthClientError.argument("sessionToken must be non-empty")
        }
        let challenge = try await powChallenge()
        let pow = try await solvePow(
            seedB64: challenge.seed,
            effortBits: challenge.effortBits
        )
        let resp: DeviceAuthorizeStartResponse = try await post(
            endpoint: "/device/authorize/start",
            body: DeviceAuthorizeStartRequest(pow: pow),
            bearer: sessionToken
        )
        return resp.grantTokenB64
    }

    /// `POST /device/authorize/redeem` — atomically consume a
    /// single-use device grant for a session-token triple (ADR-0018
    /// factor (a), fresh-device side). Mirror of the web / Android
    /// `AuthClient.deviceAuthorizeRedeem`.
    ///
    /// Anonymous (the fresh device has no token yet) + PoW-gated. A
    /// missing / expired / already-consumed grant fails as an
    /// enumeration-safe 401 (``AuthClientError/http(status:endpoint:body:)``),
    /// indistinguishable from a wrong credential by design.
    ///
    /// - Parameters:
    ///   - accountId:     Raw 16-byte account id from the pairing QR.
    ///   - grantTokenB64: URL-safe base64 (no pad) of the raw
    ///                    32-byte grant token received over the
    ///                    rendezvous channel.
    public func deviceAuthorizeRedeem(
        accountId: Data,
        grantTokenB64: String
    ) async throws -> DeviceAuthorizeRedeemResult {
        guard accountId.count == accountIdLength else {
            throw AuthClientError.argument(
                "accountId must be \(accountIdLength) bytes"
            )
        }
        guard !grantTokenB64.isEmpty else {
            throw AuthClientError.argument("grantTokenB64 must be non-empty")
        }
        let challenge = try await powChallenge()
        let pow = try await solvePow(
            seedB64: challenge.seed,
            effortBits: challenge.effortBits
        )
        let resp: DeviceAuthorizeRedeemResponse = try await post(
            endpoint: "/device/authorize/redeem",
            body: DeviceAuthorizeRedeemRequest(
                pow: pow,
                accountIdB64: base64UrlEncodeNoPad(accountId),
                grantTokenB64: grantTokenB64
            )
        )
        guard let accountIdBytes = base64UrlDecodeNoPad(resp.accountIdB64) else {
            throw AuthClientError.http(
                status: 200,
                endpoint: "/device/authorize/redeem",
                body: "malformed account_id_b64 in response"
            )
        }
        return DeviceAuthorizeRedeemResult(
            accountId: accountIdBytes,
            sessionToken: resp.sessionToken,
            sessionExpiresAtMs: resp.sessionExpiresAtMs
        )
    }

    // MARK: - TOTP — ADR-0018 factor (c), authenticator-code recovery

    /// `POST /totp/enroll/start` — mint + store an (unconfirmed) TOTP
    /// secret for this account (ADR-0018 factor (c), enrollment step
    /// 1). Mirror of the web `AuthClient.totpEnrollStart`.
    ///
    /// Bearer-authenticated (the already-logged-in account) + PoW.
    /// Returns the `otpauth://` provisioning URI (QR) + the base32
    /// secret (manual entry). The enrollment only becomes usable for
    /// recovery after ``totpEnrollFinish(sessionToken:code:)``
    /// confirms it with a live code.
    ///
    /// A 409 ``AuthClientError/http(status:endpoint:body:)`` means a
    /// CONFIRMED authenticator already exists (the server refuses a
    /// silent swap); a 503 means the server has TOTP disabled
    /// (`LAKKO_TOTP_ENC_KEY` unset).
    public func totpEnrollStart(
        sessionToken: String
    ) async throws -> TotpEnrollStartResult {
        guard !sessionToken.isEmpty else {
            throw AuthClientError.argument("sessionToken must be non-empty")
        }
        let challenge = try await powChallenge()
        let pow = try await solvePow(
            seedB64: challenge.seed,
            effortBits: challenge.effortBits
        )
        let resp: TotpEnrollStartResponse = try await post(
            endpoint: "/totp/enroll/start",
            body: TotpEnrollStartRequest(pow: pow),
            bearer: sessionToken
        )
        return TotpEnrollStartResult(
            otpauthUri: resp.otpauthUri,
            secretB32: resp.secretB32
        )
    }

    /// `POST /totp/enroll/finish` — confirm the enrollment with the
    /// first code the authenticator produced (ADR-0018 factor (c),
    /// enrollment step 2). Mirror of the web
    /// `AuthClient.totpEnrollFinish`.
    ///
    /// Bearer-authenticated, NO PoW — the server does not gate
    /// finish. A 401 ``AuthClientError/http(status:endpoint:body:)``
    /// means the code did not verify (wrong app entry / clock skew
    /// beyond +-30 s); the pending enrollment stays pending, so the
    /// user can simply retry with the next code.
    public func totpEnrollFinish(
        sessionToken: String,
        code: String
    ) async throws {
        guard !sessionToken.isEmpty else {
            throw AuthClientError.argument("sessionToken must be non-empty")
        }
        guard let normalized = normalizeTotpCode(code) else {
            throw AuthClientError.argument("code must be 6 digits")
        }
        // The server replies `{"enabled": true}`; nothing in it is
        // load-bearing for the client, so decode into an empty shape
        // (JSONDecoder ignores unknown keys).
        let _: TotpEnrollFinishAck = try await post(
            endpoint: "/totp/enroll/finish",
            body: TotpEnrollFinishRequest(code: normalized),
            bearer: sessionToken
        )
    }

    /// `POST /totp/recover` — recover ACCESS with an authenticator
    /// code (ADR-0018 factor (c), recovery side). Mirror of the web
    /// `AuthClient.totpRecover`.
    ///
    /// Anonymous (the caller lost their device) + PoW. The server is
    /// enumeration-safe by design: unknown account, unconfirmed
    /// enrollment, wrong code, and replayed code all surface the SAME
    /// generic 401 ``AuthClientError/http(status:endpoint:body:)``;
    /// 429 signals the 5-failures-in-15-minutes lockout; 503 means
    /// TOTP is disabled server-side.
    ///
    /// TOTP grants ACCESS only — the result carries no key material.
    /// Identity acquisition is the caller's job (ADR-0017 state
    /// machine).
    public func totpRecover(
        accountId: Data,
        code: String
    ) async throws -> TotpRecoverResult {
        guard accountId.count == accountIdLength else {
            throw AuthClientError.argument(
                "accountId must be \(accountIdLength) bytes"
            )
        }
        guard let normalized = normalizeTotpCode(code) else {
            throw AuthClientError.argument("code must be 6 digits")
        }
        let challenge = try await powChallenge()
        let pow = try await solvePow(
            seedB64: challenge.seed,
            effortBits: challenge.effortBits
        )
        let resp: TotpRecoverResponse = try await post(
            endpoint: "/totp/recover",
            body: TotpRecoverRequest(
                pow: pow,
                accountIdB64: base64UrlEncodeNoPad(accountId),
                code: normalized
            )
        )
        guard let accountIdBytes = base64UrlDecodeNoPad(resp.accountIdB64) else {
            throw AuthClientError.http(
                status: 200,
                endpoint: "/totp/recover",
                body: "malformed account_id_b64 in response"
            )
        }
        return TotpRecoverResult(
            accountId: accountIdBytes,
            sessionToken: resp.sessionToken,
            sessionExpiresAtMs: resp.sessionExpiresAtMs
        )
    }

    // MARK: - High-level: full signup / login / recover flows

    /// Drive the full signup flow:
    ///
    /// ```text
    ///   POST /pow/challenge   →  seed₁
    ///   client: solve PoW + OPAQUE password start [+ recovery start]
    ///   POST /signup/start    →  password_response [+ recovery_response]
    ///   client: OPAQUE finish (both flows)
    ///   POST /pow/challenge   →  seed₂
    ///   client: solve PoW
    ///   POST /signup/finish   →  ok
    /// ```
    ///
    /// When `enrolRecovery` is `true` (the default), the method also
    /// generates a fresh BIP39 24-word mnemonic, enrols it as a
    /// parallel OPAQUE envelope under `KIND_RECOVERY` server-side,
    /// and returns the phrase in ``SignupResult/recoveryPhrase`` for
    /// the UI to display **once**. Both envelopes commit atomically
    /// inside `/signup/finish` (server transaction).
    public func signup(
        password: Data,
        enrolRecovery: Bool = true
    ) async throws -> SignupResult {
        guard !password.isEmpty else {
            throw AuthClientError.argument("password must be non-empty")
        }

        let accountId = generateAccountId()
        let accountIdB64 = base64Encode(accountId)

        // ------------- start
        let challenge1 = try await powChallenge()
        let pow1 = try await solvePow(
            seedB64: challenge1.seed,
            effortBits: challenge1.effortBits
        )

        let passwordReg = try OpaqueRegister.startPassword(password: password)

        // Optional recovery enrolment. The Kotlin side splits this
        // into a `RecoveryBundle` private struct; in Swift we keep
        // three separate locals + a guard to derive a `recoveryEnabled`
        // flag — fewer types, same readability.
        var recoveryMnemonic: RecoveryMnemonic?
        var recoverySeed: Data?
        var recoveryReg: OpaqueRegister?
        if enrolRecovery {
            let mnemonic = RecoveryMnemonic.generate()
            let seed = mnemonic.toSeed()
            let reg = try OpaqueRegister.startRecovery(seed: seed)
            recoveryMnemonic = mnemonic
            recoverySeed = seed
            recoveryReg = reg
        }

        let startResp = try await signupStart(
            SignupStartRequest(
                pow: pow1,
                accountId: accountIdB64,
                passwordRequest: passwordReg.requestB64(),
                recoveryRequest: recoveryReg?.requestB64()
            )
        )

        if recoveryReg != nil, startResp.recoveryResponse == nil {
            // Server should always echo back a recovery_response when
            // we set recovery_request. Treat the missing field as a
            // server-side wiring bug rather than crashing later in
            // OPAQUE finish.
            throw AuthClientError.http(
                status: 200,
                endpoint: "/signup/start",
                body: "missing recovery_response despite recovery_request being set"
            )
        }

        // ------------- finish (OPAQUE math)
        let passwordOut = try passwordReg.finish(
            secret: password,
            serverResponseB64: startResp.passwordResponse
        )

        var recoveryUploadB64: String?
        if let reg = recoveryReg,
           let seed = recoverySeed,
           let response = startResp.recoveryResponse
        {
            let out = try reg.finish(secret: seed, serverResponseB64: response)
            recoveryUploadB64 = out.uploadB64()
        }

        let challenge2 = try await powChallenge()
        let pow2 = try await solvePow(
            seedB64: challenge2.seed,
            effortBits: challenge2.effortBits
        )

        let finishResp = try await signupFinish(
            SignupFinishRequest(
                pow: pow2,
                accountId: accountIdB64,
                passwordUpload: passwordOut.uploadB64(),
                recoveryUpload: recoveryUploadB64
            )
        )
        guard finishResp.ok else {
            throw AuthClientError.http(
                status: 200,
                endpoint: "/signup/finish",
                body: "server reported ok=false"
            )
        }

        return SignupResult(
            accountId: accountId,
            exportKey: passwordOut.exportKey(),
            recoveryPhrase: recoveryMnemonic?.phrase(),
            sessionToken: finishResp.sessionToken,
            sessionExpiresAtMs: finishResp.sessionExpiresAtMs
        )
    }

    /// Drive the full password-login flow:
    ///
    /// ```text
    ///   POST /pow/challenge  →  seed
    ///   client: solve PoW + OPAQUE start
    ///   POST /login/start    →  session_id, response
    ///   client: OPAQUE finish
    ///   POST /login/finish   →  ok, account_id
    /// ```
    ///
    /// `accountId` MUST be the raw 16-byte id the server gave back
    /// at signup time. The SwiftUI side will pin it in the Keychain
    /// keyed by username (1.14d2).
    ///
    /// Wrong passwords surface as an
    /// ``AuthClientError/http(status:endpoint:body:)`` with status
    /// 401 — by design, indistinguishable from "unknown account".
    public func login(
        accountId: Data,
        password: Data
    ) async throws -> LoginResult {
        guard !password.isEmpty else {
            throw AuthClientError.argument("password must be non-empty")
        }
        guard accountId.count == accountIdLength else {
            throw AuthClientError.argument(
                "accountId must be \(accountIdLength) bytes"
            )
        }

        let accountIdB64 = base64Encode(accountId)
        let challenge = try await powChallenge()
        let pow = try await solvePow(
            seedB64: challenge.seed,
            effortBits: challenge.effortBits
        )

        let login = try OpaqueLogin.startPassword(password: password)
        let startResp = try await loginStart(
            LoginStartRequest(
                pow: pow,
                accountId: accountIdB64,
                request: login.requestB64()
            )
        )

        let out = try login.finish(
            secret: password,
            serverResponseB64: startResp.response
        )
        let finishResp = try await loginFinish(
            LoginFinishRequest(
                sessionId: startResp.sessionId,
                finalize: out.finalizeB64()
            )
        )
        guard finishResp.ok else {
            throw AuthClientError.http(
                status: 200,
                endpoint: "/login/finish",
                body: "server reported ok=false"
            )
        }
        return LoginResult(
            accountId: accountId,
            sessionKey: out.sessionKey(),
            exportKey: out.exportKey(),
            sessionToken: finishResp.sessionToken,
            sessionExpiresAtMs: finishResp.sessionExpiresAtMs
        )
    }

    /// Drive the full recover-from-mnemonic flow:
    ///
    /// ```text
    ///   POST /pow/challenge          →  seed
    ///   client: solve PoW
    ///   client: OPAQUE recovery start (seed)
    ///   client: OPAQUE password start (newPassword)
    ///   POST /password/reset/start   →  session_id, recovery_response,
    ///                                    new_password_response
    ///   client: OPAQUE recovery finish + OPAQUE password finish
    ///   POST /password/reset/finish  →  ok, account_id
    /// ```
    ///
    /// Drives the `/password/reset/{start,finish}` endpoint pair.
    /// The server proves recovery ownership via the mnemonic and
    /// atomically replaces the `KIND_PASSWORD` envelope with the
    /// new one — the `KIND_RECOVERY` envelope stays intact, so the
    /// same 24 words work for future resets.
    ///
    /// Note: the recovery flow only requires **one** PoW solve
    /// (gating `/start`), unlike signup which needs two.
    /// Wrong-mnemonic attempts fail at the AKE finalization with
    /// the same indistinguishable response as `/recovery/login/finish`.
    public func recover(
        accountId: Data,
        mnemonicSeed: Data,
        newPassword: Data
    ) async throws -> RecoverResult {
        guard accountId.count == accountIdLength else {
            throw AuthClientError.argument(
                "accountId must be \(accountIdLength) bytes"
            )
        }
        guard !mnemonicSeed.isEmpty else {
            throw AuthClientError.argument("mnemonicSeed must be non-empty")
        }
        guard !newPassword.isEmpty else {
            throw AuthClientError.argument("newPassword must be non-empty")
        }

        let accountIdB64 = base64Encode(accountId)

        // Single PoW solve — the server only gates /start.
        let challenge = try await powChallenge()
        let pow = try await solvePow(
            seedB64: challenge.seed,
            effortBits: challenge.effortBits
        )

        // Two parallel OPAQUE flows: recovery login proves the user
        // knows the mnemonic; password registration enrols the new
        // password envelope. Both are independent — neither needs
        // the other's intermediate state.
        let recoveryLogin = try OpaqueLogin.startRecovery(seed: mnemonicSeed)
        let newPasswordReg = try OpaqueRegister.startPassword(password: newPassword)

        let startResp = try await passwordResetStart(
            PasswordResetStartRequest(
                pow: pow,
                accountId: accountIdB64,
                recoveryRequest: recoveryLogin.requestB64(),
                newPasswordRequest: newPasswordReg.requestB64()
            )
        )

        // Both finish steps. A wrong mnemonic seed here surfaces as
        // an OPAQUE-side error at `recoveryLogin.finish` (the AKE
        // can't agree on a session key); we surface it to the caller
        // unchanged so the form can render "invalid recovery phrase"
        // without distinguishing further.
        let recoveryOut = try recoveryLogin.finish(
            secret: mnemonicSeed,
            serverResponseB64: startResp.recoveryResponse
        )
        let passwordOut = try newPasswordReg.finish(
            secret: newPassword,
            serverResponseB64: startResp.newPasswordResponse
        )

        let finishResp = try await passwordResetFinish(
            PasswordResetFinishRequest(
                sessionId: startResp.sessionId,
                recoveryFinalize: recoveryOut.finalizeB64(),
                newPasswordUpload: passwordOut.uploadB64()
            )
        )
        guard finishResp.ok else {
            throw AuthClientError.http(
                status: 200,
                endpoint: "/password/reset/finish",
                body: "server reported ok=false"
            )
        }

        return RecoverResult(
            accountId: accountId,
            exportKey: passwordOut.exportKey(),
            sessionToken: finishResp.sessionToken,
            sessionExpiresAtMs: finishResp.sessionExpiresAtMs
        )
    }

    // MARK: - Internal HTTP helpers

    private func post<Req: Encodable, Res: Decodable>(
        endpoint: String,
        body: Req,
        bearer: String? = nil
    ) async throws -> Res {
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        if let bearer {
            request.setValue(
                "Bearer \(bearer)",
                forHTTPHeaderField: "Authorization"
            )
        }
        do {
            request.httpBody = try encoder.encode(body)
        } catch {
            throw AuthClientError.argument(
                "failed to encode \(endpoint) body: \(error)"
            )
        }
        return try await execute(endpoint: endpoint, request: request)
    }

    private func postNoBody<Res: Decodable>(
        endpoint: String
    ) async throws -> Res {
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        return try await execute(endpoint: endpoint, request: request)
    }

    private func buildURL(endpoint: String) throws -> URL {
        guard let url = URL(string: baseURL + endpoint) else {
            throw AuthClientError.argument(
                "invalid baseURL '\(baseURL)' or endpoint '\(endpoint)'"
            )
        }
        return url
    }

    private func execute<Res: Decodable>(
        endpoint: String,
        request: URLRequest
    ) async throws -> Res {
        let raw: (Data, URLResponse)
        do {
            raw = try await session.data(for: request)
        } catch {
            throw AuthClientError.network(endpoint: endpoint, underlying: error)
        }

        let (data, response) = raw
        guard let httpResponse = response as? HTTPURLResponse else {
            // URLSession always hands back HTTPURLResponse for an
            // http(s) URL, but the generic protocol contract is
            // `URLResponse`. Defensive guard for the type cast.
            throw AuthClientError.http(
                status: 0,
                endpoint: endpoint,
                body: "non-HTTP response"
            )
        }
        guard (200..<300).contains(httpResponse.statusCode) else {
            let bodyText = truncate(String(data: data, encoding: .utf8) ?? "<binary>")
            throw AuthClientError.http(
                status: httpResponse.statusCode,
                endpoint: endpoint,
                body: bodyText
            )
        }
        do {
            return try decoder.decode(Res.self, from: data)
        } catch {
            let bodyText = truncate(String(data: data, encoding: .utf8) ?? "<binary>")
            throw AuthClientError.http(
                status: httpResponse.statusCode,
                endpoint: endpoint,
                body: "decode failed: \(error) [body=\(bodyText)]"
            )
        }
    }

    private func truncate(_ s: String, max: Int = 512) -> String {
        if s.count > max {
            return String(s.prefix(max)) + "…"
        }
        return s
    }
}

// MARK: - Result types

/// Result of ``AuthClient/signup(password:enrolRecovery:)``.
public struct SignupResult: Equatable {
    /// Raw 16 bytes — the canonical id the server returned and that
    /// the client must use on every subsequent login.
    public let accountId: Data
    /// OPAQUE export key bytes (64 for `LakkoCipherSuite`). The
    /// device-root key gets derived from this in 1.14d2 — the
    /// caller MUST handle it carefully (no logging, no `UserDefaults`).
    public let exportKey: Data
    /// The 24-word recovery phrase, present when signup was called
    /// with `enrolRecovery = true` (the default). The UI MUST
    /// display it exactly once and ask the user to record it
    /// somewhere safe — losing it means losing the only way back
    /// into the account if the password is forgotten.
    public let recoveryPhrase: String?
    /// Phase 1.5b: bearer token issued by the auth service. Carry
    /// in the `Authorization: Bearer <token>` header on every
    /// protected `lakko-svc-discovery` write.
    public let sessionToken: String
    /// Token expiry as a unix-millisecond timestamp.
    public let sessionExpiresAtMs: Int64

    public init(
        accountId: Data,
        exportKey: Data,
        recoveryPhrase: String? = nil,
        sessionToken: String,
        sessionExpiresAtMs: Int64
    ) {
        self.accountId = accountId
        self.exportKey = exportKey
        self.recoveryPhrase = recoveryPhrase
        self.sessionToken = sessionToken
        self.sessionExpiresAtMs = sessionExpiresAtMs
    }
}

/// Result of ``AuthClient/login(accountId:password:)``.
public struct LoginResult: Equatable {
    public let accountId: Data
    /// Symmetric session key shared with the server. Use to bind a
    /// follow-up session token; **not** to encrypt user data.
    public let sessionKey: Data
    /// Same export key the user got at signup time — deterministic
    /// for the same password.
    public let exportKey: Data
    /// Phase 1.5b: bearer token issued by the auth service.
    public let sessionToken: String
    /// Token expiry as a unix-millisecond timestamp.
    public let sessionExpiresAtMs: Int64

    public init(
        accountId: Data,
        sessionKey: Data,
        exportKey: Data,
        sessionToken: String,
        sessionExpiresAtMs: Int64
    ) {
        self.accountId = accountId
        self.sessionKey = sessionKey
        self.exportKey = exportKey
        self.sessionToken = sessionToken
        self.sessionExpiresAtMs = sessionExpiresAtMs
    }
}

/// Result of ``AuthClient/recover(accountId:mnemonicSeed:newPassword:)``.
public struct RecoverResult: Equatable {
    public let accountId: Data
    /// OPAQUE export key for the **new** password. Derive a fresh
    /// device root key — the previous device key (if any) is no
    /// longer reachable from the new password.
    public let exportKey: Data
    /// Phase 1.5b: fresh bearer token issued by the auth service.
    public let sessionToken: String
    /// Token expiry as a unix-millisecond timestamp.
    public let sessionExpiresAtMs: Int64

    public init(
        accountId: Data,
        exportKey: Data,
        sessionToken: String,
        sessionExpiresAtMs: Int64
    ) {
        self.accountId = accountId
        self.exportKey = exportKey
        self.sessionToken = sessionToken
        self.sessionExpiresAtMs = sessionExpiresAtMs
    }
}

/// Result of ``AuthClient/deviceAuthorizeRedeem(accountId:grantTokenB64:)``
/// — the same `(account_id, session_token, expiry)` triple every other
/// login flow returns (ADR-0018 factor (a)).
public struct DeviceAuthorizeRedeemResult: Equatable {
    /// Raw 16-byte account id echoed back by the server.
    public let accountId: Data
    /// Bearer token issued by the auth service — the fresh device's
    /// only ACCESS on the no-password restore path.
    public let sessionToken: String
    /// Token expiry as a unix-millisecond timestamp.
    public let sessionExpiresAtMs: Int64

    public init(
        accountId: Data,
        sessionToken: String,
        sessionExpiresAtMs: Int64
    ) {
        self.accountId = accountId
        self.sessionToken = sessionToken
        self.sessionExpiresAtMs = sessionExpiresAtMs
    }
}

/// Result of ``AuthClient/totpEnrollStart(sessionToken:)`` (ADR-0018
/// factor (c)). Provisioning material for the authenticator app —
/// display the QR + the manual-entry secret exactly once; neither is
/// persisted client-side.
public struct TotpEnrollStartResult: Equatable {
    /// `otpauth://totp/...` provisioning URI — render as a QR (RAW
    /// text, not base64url-wrapped bytes — the authenticator app must
    /// see the URI verbatim).
    public let otpauthUri: String
    /// RFC 4648 base32 (no padding) secret for manual entry.
    public let secretB32: String

    public init(otpauthUri: String, secretB32: String) {
        self.otpauthUri = otpauthUri
        self.secretB32 = secretB32
    }
}

/// Result of ``AuthClient/totpRecover(accountId:code:)`` (ADR-0018
/// factor (c)) — the same `(account_id, session_token, expiry)`
/// triple every other login flow returns. ACCESS only, no identity
/// material.
public struct TotpRecoverResult: Equatable {
    /// Raw 16-byte account id echoed back by the server.
    public let accountId: Data
    public let sessionToken: String
    public let sessionExpiresAtMs: Int64

    public init(
        accountId: Data,
        sessionToken: String,
        sessionExpiresAtMs: Int64
    ) {
        self.accountId = accountId
        self.sessionToken = sessionToken
        self.sessionExpiresAtMs = sessionExpiresAtMs
    }
}

/// Throwaway decode target for the `/totp/enroll/finish` ack
/// (`{"enabled": true}`) — nothing in the body is load-bearing, and
/// `JSONDecoder` ignores unknown keys for an empty shape.
private struct TotpEnrollFinishAck: Decodable {}

// MARK: - Errors

/// Error vocabulary for ``AuthClient``.
///
/// `Equatable` synthesis covers the `argument` and `http` cases
/// trivially; the `network` case holds an `Error` (existential, not
/// `Equatable`) so we hand-roll the equality below.
public enum AuthClientError: Error {
    /// A precondition was violated by the caller — e.g. an empty
    /// password. Distinct from a network error so the UI can surface
    /// it as a form validation message.
    case argument(String)
    /// A request to the auth service did not return 2xx. The
    /// original status code and (truncated) body are exposed for
    /// diagnostics.
    case http(status: Int, endpoint: String, body: String)
    /// Network failure (DNS / connectivity / TLS). Distinct from
    /// `http` so the UI can suggest "check connection" rather than
    /// "wrong credentials".
    case network(endpoint: String, underlying: Error)
}

extension AuthClientError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .argument(let message):
            return message
        case .http(let status, let endpoint, let body):
            return "auth \(endpoint) → \(status): \(body)"
        case .network(let endpoint, let underlying):
            return "auth \(endpoint) network error: \(underlying)"
        }
    }
}
