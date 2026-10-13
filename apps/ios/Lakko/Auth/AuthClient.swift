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
///      ``login(handle:password:)`` /
///      ``recover(handle:mnemonicSeed:newPassword:)``** orchestrate
///      the full multi-round-trip flow:
///      `/pow/challenge` → `/signup/start` → `/signup/finish` (and the
///      equivalent for login and recover, which carry no proof of work
///      since ADR-0054 S4).
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
/// Derived from the build's API base URL (``ApiEndpoints``, Info.plist
/// `LakkoApiBaseUrl`): `<base>/auth`. With no base URL configured it is
/// `http://localhost:8081` — the canonical "host machine from inside
/// the iOS simulator" address (the simulator shares the host's
/// network namespace, unlike the Android emulator which needs
/// `10.0.2.2`).
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

    /// Default base URL — `<LakkoApiBaseUrl>/auth`, or the
    /// `lakko-svc-auth` dev bind `http://localhost:8081` when the build
    /// carries no API base URL. See ``ApiEndpoints``.
    public static var defaultBaseURL: String { ApiEndpoints.baseURL(for: .auth) }

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

    /// The proof-of-work solver signup runs. Injectable so a test can hold
    /// the solve open and cancel it.
    public typealias PowSolver = @Sendable (_ seedB64: String, _ effortBits: Int) async throws -> PowProofPayload
    private let solve: PowSolver

    public init(
        baseURL: String = AuthClient.defaultBaseURL,
        session: URLSession = .shared,
        solve: @escaping PowSolver = { try await solvePow(seedB64: $0, effortBits: $1) }
    ) {
        self.baseURL = baseURL
        self.session = session
        self.solve = solve
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
    /// holds a session). Returns the URL-safe base64
    /// (no pad) of the RAW grant token — the server stores only its
    /// SHA-256. The caller seals it (together with the entropy of the
    /// recovery phrase) to the fresh device over the E2EE pairing channel; it never
    /// travels in plaintext past this response.
    ///
    /// `freshGrant` is the fresh-auth grant (ADR-0073, amended 2026-10-06):
    /// without a valid one the server answers `403 fresh-auth-required` and
    /// mints nothing.
    public func deviceAuthorizeStart(sessionToken: String, freshGrant: String) async throws -> String {
        guard !sessionToken.isEmpty else {
            throw AuthClientError.argument("sessionToken must be non-empty")
        }
        let resp: DeviceAuthorizeStartResponse = try await post(
            endpoint: "/device/authorize/start",
            body: DeviceAuthorizeStartRequest(freshGrant: freshGrant),
            bearer: sessionToken
        )
        return resp.grantTokenB64
    }

    /// `POST /device/authorize/redeem` — atomically consume a
    /// single-use device grant for a session-token triple (ADR-0018
    /// factor (a), fresh-device side). Mirror of the web / Android
    /// `AuthClient.deviceAuthorizeRedeem`.
    ///
    /// Anonymous (the fresh device has no token yet) and, since ADR-0056,
    /// carries the grant only: the fresh device learns its account id from
    /// the response, after proving the grant. Limited per address
    /// server-side (ADR-0054 S4); a missing / expired / already-consumed
    /// grant fails as an enumeration-safe 401
    /// (``AuthClientError/http(status:endpoint:body:)``).
    ///
    /// - Parameter grantTokenB64: URL-safe base64 (no pad) of the raw
    ///   32-byte grant token received over the rendezvous channel.
    public func deviceAuthorizeRedeem(
        grantTokenB64: String
    ) async throws -> DeviceAuthorizeRedeemResult {
        guard !grantTokenB64.isEmpty else {
            throw AuthClientError.argument("grantTokenB64 must be non-empty")
        }
        let resp: DeviceAuthorizeRedeemResponse = try await post(
            endpoint: "/device/authorize/redeem",
            body: DeviceAuthorizeRedeemRequest(grantTokenB64: grantTokenB64)
        )
        guard let accountIdBytes = base64UrlDecodeNoPad(resp.accountIdB64),
              accountIdBytes.count == accountIdLength else {
            throw AuthClientError.http(
                status: 200,
                endpoint: "/device/authorize/redeem",
                body: "malformed account_id_b64 in response"
            )
        }
        return DeviceAuthorizeRedeemResult(accountId: accountIdBytes, session: resp.sessionPair)
    }

    // MARK: - Genesis attestation — ADR-0036 (G-30 finding 2, anti-squatting)

    /// `POST /auth/genesis/attest` — mint a signed `GenesisAttestation`
    /// binding the OPAQUE-authenticated account to `identityPubkey` (ADR-0036).
    /// Mirror of the web / Android `AuthClient.mintGenesisAttestation`.
    ///
    /// Bearer-authenticated (the session token is the proof the caller
    /// OPAQUE-authenticated as its `account_id` — the whole security basis for
    /// anti-squatting: only the account holder can mint for their own account).
    /// NO PoW. The client uploads the returned bytes alongside the identity
    /// bundle so the transparency genesis entry is account-authenticated.
    ///
    /// Returns the RAW 153-byte attestation (URL-safe-no-pad decoded from the
    /// response), ready to hand to `DiscoveryClient.uploadIdentityBundle`.
    public func mintGenesisAttestation(
        sessionToken: String,
        identityPubkey: Data
    ) async throws -> Data {
        guard !sessionToken.isEmpty else {
            throw AuthClientError.argument("sessionToken must be non-empty")
        }
        guard identityPubkey.count == 32 else {
            throw AuthClientError.argument("identityPubkey must be 32 bytes (Ed25519)")
        }
        let resp: GenesisAttestResponse = try await post(
            endpoint: "/auth/genesis/attest",
            body: GenesisAttestRequest(
                pubkeyB64: base64UrlEncodeNoPad(identityPubkey)
            ),
            bearer: sessionToken
        )
        guard let attestation = base64UrlDecodeNoPad(resp.genesisAttestB64) else {
            throw AuthClientError.http(
                status: 200,
                endpoint: "/auth/genesis/attest",
                body: "malformed genesis_attest_b64 in response"
            )
        }
        return attestation
    }

    // MARK: - TOTP — ADR-0018 factor (c), authenticator-code recovery

    /// `POST /totp/enroll/start` — mint + store an (unconfirmed) TOTP
    /// secret for this account (ADR-0018 factor (c), enrollment step
    /// 1). Mirror of the web `AuthClient.totpEnrollStart`.
    ///
    /// Bearer-authenticated (the already-logged-in account).
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
        let resp: TotpEnrollStartResponse = try await postNoBody(
            endpoint: "/totp/enroll/start",
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
    /// Anonymous (the caller lost their device). The server is
    /// enumeration-safe by design: unknown account, unconfirmed
    /// enrollment, wrong code, and replayed code all surface the SAME
    /// generic 401 ``AuthClientError/http(status:endpoint:body:)``;
    /// 429 signals the per-name / per-address limit (``AuthRateLimit``); 503 means
    /// TOTP is disabled server-side.
    ///
    /// TOTP grants ACCESS only — the result carries no key material.
    /// Identity acquisition is the caller's job (ADR-0017 state
    /// machine).
    public func totpRecover(
        handle: Data,
        code: String
    ) async throws -> TotpRecoverResult {
        try requireHandle(handle)
        guard let normalized = normalizeTotpCode(code) else {
            throw AuthClientError.argument("code must be 6 digits")
        }
        let resp: TotpRecoverResponse = try await post(
            endpoint: "/totp/recover",
            body: TotpRecoverRequest(
                handle: base64UrlEncodeNoPad(handle),
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
        return TotpRecoverResult(accountId: accountIdBytes, session: resp.sessionPair)
    }

    // MARK: - High-level: full signup / login / recover flows

    /// Drive the full signup flow:
    ///
    /// ```text
    ///   POST /pow/challenge   →  seed (the only proof of work left, ADR-0054)
    ///   client: solve PoW (cancellable) + OPAQUE password start [+ recovery start]
    ///   POST /signup/start    →  password_response [+ recovery_response]
    ///   client: OPAQUE finish (both flows)
    ///   POST /signup/finish   →  ok
    /// ```
    ///
    /// When `enrolRecovery` is `true` (the default), the method also
    /// generates a fresh BIP39 12-word mnemonic, enrols it as a
    /// parallel OPAQUE envelope under `KIND_RECOVERY` server-side,
    /// and returns the phrase in ``SignupResult/recoveryPhrase`` for
    /// the UI to display **once**. Both envelopes commit atomically
    /// inside `/signup/finish` (server transaction).
    ///
    /// ADR-0054 S4: the proof of work before `/signup/start` is the only one
    /// left in the app and deliberately heavy. The flow is cancellable up to
    /// the finish: a cancelled task stops before `/signup/start` or before
    /// `/signup/finish` (``CancellationError``), so no account is created.
    /// `onCommitting` runs right before `/signup/finish` - past it the account
    /// is being created and the caller should no longer offer Cancel.
    /// A `410` from the finish (no live start for this account, e.g. more
    /// than 600 s between the two) restarts the whole signup once from the
    /// start.
    ///
    /// ADR-0074 section 0.4: `deviceIdFor` is asked, with the account id this
    /// signup generated, for the id of this device (its conversation leaf
    /// key); the finish asserts it, so the session names the device from the
    /// first token. `nil`, or a `nil` answer, lands an unbound session.
    public func signup(
        password: Data,
        enrolRecovery: Bool = true,
        deviceIdFor: (@Sendable (_ accountId: Data) async -> Data?)? = nil,
        onCommitting: (@MainActor () -> Void)? = nil
    ) async throws -> SignupResult {
        guard !password.isEmpty else {
            throw AuthClientError.argument("password must be non-empty")
        }
        do {
            return try await signupOnce(
                password: password, enrolRecovery: enrolRecovery,
                deviceIdFor: deviceIdFor, onCommitting: onCommitting)
        } catch AuthClientError.http(let status, let endpoint, _)
            where status == 410 && endpoint == "/signup/finish" {
            NSLog("[auth] signup finish found no live start (410), restarting from the start")
            return try await signupOnce(
                password: password, enrolRecovery: enrolRecovery,
                deviceIdFor: deviceIdFor, onCommitting: onCommitting)
        }
    }

    private func signupOnce(
        password: Data,
        enrolRecovery: Bool,
        deviceIdFor: (@Sendable (_ accountId: Data) async -> Data?)?,
        onCommitting: (@MainActor () -> Void)?
    ) async throws -> SignupResult {
        let accountId = generateAccountId()
        let accountIdB64 = base64Encode(accountId)

        // ------------- start
        try Task.checkCancellation()
        let challenge1 = try await powChallenge()
        let pow1 = try await solve(challenge1.seed, challenge1.effortBits)
        try Task.checkCancellation()

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

        try Task.checkCancellation()
        if let onCommitting { await onCommitting() }
        let deviceId = LandingDevice.usable(await deviceIdFor?(accountId))
        let finishResp = try await signupFinish(
            SignupFinishRequest(
                accountId: accountIdB64,
                passwordUpload: passwordOut.uploadB64(),
                recoveryUpload: recoveryUploadB64,
                deviceIdB64: deviceId.map(base64UrlEncodeNoPad)
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
            session: finishResp.sessionPair.naming(deviceId: deviceId),
            freshGrant: FreshAuthGrant.from(
                grant: finishResp.freshGrant, expiresAtMs: finishResp.freshGrantExpiresAtMs)
        )
    }

    /// Drive the full password-login flow:
    ///
    /// ```text
    ///   client: OPAQUE start
    ///   POST /login/start    →  session_id, response
    ///   client: OPAQUE finish
    ///   POST /login/finish   →  ok, account_id
    /// ```
    ///
    /// ADR-0054 S5: the account is named by its 32-byte login `handle`
    /// (``LoginName``); the account id is learned from the finish response,
    /// never sent.
    ///
    /// Wrong passwords surface as an
    /// ``AuthClientError/http(status:endpoint:body:)`` with status
    /// 401 — by design, indistinguishable from "unknown account" (a handle
    /// nobody holds runs the same path on a stand-in account).
    ///
    /// ADR-0074 section 0.4: `deviceId` is asserted at the finish when the
    /// caller knows the account before it (the proof login of a signed-in
    /// account); `nil` lands an unbound session the caller binds afterwards.
    public func login(
        handle: Data,
        password: Data,
        deviceId: Data? = nil
    ) async throws -> LoginResult {
        guard !password.isEmpty else {
            throw AuthClientError.argument("password must be non-empty")
        }
        try requireHandle(handle)

        let login = try OpaqueLogin.startPassword(password: password)
        let startResp = try await loginStart(
            LoginStartRequest(
                handle: base64UrlEncodeNoPad(handle),
                request: login.requestB64()
            )
        )

        let out = try login.finish(
            secret: password,
            serverResponseB64: startResp.response
        )
        let deviceId = LandingDevice.usable(deviceId)
        let finishResp = try await loginFinish(
            LoginFinishRequest(
                sessionId: startResp.sessionId,
                finalize: out.finalizeB64(),
                deviceIdB64: deviceId.map(base64UrlEncodeNoPad)
            )
        )
        guard finishResp.ok else {
            throw AuthClientError.http(
                status: 200,
                endpoint: "/login/finish",
                body: "server reported ok=false"
            )
        }
        let accountId = try Self.decodeAccountId(finishResp.accountId, endpoint: "/login/finish")
        return LoginResult(
            accountId: accountId,
            sessionKey: out.sessionKey(),
            exportKey: out.exportKey(),
            session: finishResp.sessionPair.naming(deviceId: deviceId),
            freshGrant: FreshAuthGrant.from(
                grant: finishResp.freshGrant, expiresAtMs: finishResp.freshGrantExpiresAtMs)
        )
    }

    /// Drive the full recover-from-mnemonic flow:
    ///
    /// ```text
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
    /// same 12 words work for future resets.
    ///
    /// Wrong-mnemonic attempts fail at the AKE finalization with
    /// the same indistinguishable response as `/recovery/login/finish`.
    public func recover(
        handle: Data,
        mnemonicSeed: Data,
        newPassword: Data
    ) async throws -> RecoverResult {
        try requireHandle(handle)
        guard !mnemonicSeed.isEmpty else {
            throw AuthClientError.argument("mnemonicSeed must be non-empty")
        }
        guard !newPassword.isEmpty else {
            throw AuthClientError.argument("newPassword must be non-empty")
        }

        // Two parallel OPAQUE flows: recovery login proves the user
        // knows the mnemonic; password registration enrols the new
        // password envelope. Both are independent — neither needs
        // the other's intermediate state.
        let recoveryLogin = try OpaqueLogin.startRecovery(seed: mnemonicSeed)
        let newPasswordReg = try OpaqueRegister.startPassword(password: newPassword)

        let startResp = try await passwordResetStart(
            PasswordResetStartRequest(
                handle: base64UrlEncodeNoPad(handle),
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
        let accountId = try Self.decodeAccountId(
            finishResp.accountId, endpoint: "/password/reset/finish")

        return RecoverResult(
            accountId: accountId,
            exportKey: passwordOut.exportKey(),
            session: finishResp.sessionPair
        )
    }

    /// Sign in with the recovery credential, replacing nothing:
    ///
    /// ```text
    ///   client: OPAQUE recovery start (seed)
    ///   POST /recovery/login/start   ->  session_id, response
    ///   client: OPAQUE recovery finish
    ///   POST /recovery/login/finish  ->  ok, account_id, session
    /// ```
    ///
    /// `seed` is the 64-byte BIP39 seed of the recovery phrase. The password
    /// envelope is untouched, which is the difference from
    /// ``recover(handle:mnemonicSeed:newPassword:)``. Used by the local
    /// three-word check: a device that holds the phrase and saw three of
    /// its words typed correctly signs in with the seed it already has.
    ///
    /// A seed that is not the account's recovery credential, and a name
    /// nobody holds, answer the same `401`: a local finish failure (the
    /// exchange cannot be completed on this side) is reported as that `401`
    /// too, so callers see one refusal whichever side noticed.
    public func recoveryLogin(
        handle: Data,
        seed: Data
    ) async throws -> TotpRecoverResult {
        try requireHandle(handle)
        guard !seed.isEmpty else {
            throw AuthClientError.argument("seed must be non-empty")
        }
        let login = try OpaqueLogin.startRecovery(seed: seed)
        let startResp = try await recoveryLoginStart(
            LoginStartRequest(
                handle: base64UrlEncodeNoPad(handle),
                request: login.requestB64()
            )
        )
        let finalizeB64: String
        do {
            finalizeB64 = try login.finish(
                secret: seed, serverResponseB64: startResp.response
            ).finalizeB64()
        } catch {
            throw AuthClientError.http(
                status: 401,
                endpoint: "/recovery/login/finish",
                body: "recovery credential did not match"
            )
        }
        let finishResp = try await recoveryLoginFinish(
            LoginFinishRequest(sessionId: startResp.sessionId, finalize: finalizeB64)
        )
        guard finishResp.ok else {
            throw AuthClientError.http(
                status: 200,
                endpoint: "/recovery/login/finish",
                body: "server reported ok=false"
            )
        }
        return TotpRecoverResult(
            accountId: try Self.decodeAccountId(
                finishResp.accountId, endpoint: "/recovery/login/finish"),
            session: finishResp.sessionPair
        )
    }

    // MARK: - Session refresh + the key reset (ADR-0073)

    /// `POST /auth/session/refresh` — trade the refresh credential for a new
    /// session pair (ADR-0073 section 3.2). Anonymous: the credential is the
    /// authorisation. Per-address limited.
    ///
    /// Throws ``AuthClientError/http(status:endpoint:body:)`` with status
    /// `401` and the body `{"error":"key-generation-superseded"}` when the
    /// account's keys were reset elsewhere, or `{"error":"refresh-invalid"}`
    /// for a bad, expired or unknown credential; the provider reads the
    /// body (``SessionTokenRule/classify401(body:)``).
    public func refreshSession(refreshToken: String) async throws -> SessionPair {
        guard !refreshToken.isEmpty else {
            throw AuthClientError.argument("refreshToken must be non-empty")
        }
        let resp: SessionRefreshResponse = try await post(
            endpoint: "/auth/session/refresh",
            body: SessionRefreshRequest(refreshToken: refreshToken)
        )
        return resp.sessionPair
    }

    // MARK: - ADR-0074: the session names a device

    /// `POST /auth/session/bind-device` (bearer) — name the device a session
    /// that landed without one belongs to (ADR-0074 "T1 as built" points 8
    /// and 9). `refreshToken` is the refresh credential of the same session.
    /// The returned pair names the device; the caller drops the pair it
    /// replaced.
    ///
    /// Throws ``AuthClientError/http(status:endpoint:body:)``: `400` (a bad or
    /// all-zero id), `401 refresh-invalid`, `401 key-generation-superseded`,
    /// `401 device-revoked`, `409 device-already-bound`.
    public func bindDevice(
        sessionToken: String,
        deviceId: Data,
        refreshToken: String
    ) async throws -> SessionPair {
        guard !sessionToken.isEmpty, !refreshToken.isEmpty else {
            throw AuthClientError.argument("sessionToken and refreshToken must be non-empty")
        }
        guard let deviceId = LandingDevice.usable(deviceId) else {
            throw AuthClientError.argument("deviceId must be 32 bytes and not all zero")
        }
        let resp: SessionRefreshResponse = try await post(
            endpoint: "/auth/session/bind-device",
            body: BindDeviceRequest(
                deviceIdB64: base64UrlEncodeNoPad(deviceId), refreshToken: refreshToken),
            bearer: sessionToken
        )
        return resp.sessionPair.naming(deviceId: deviceId)
    }

    /// `POST /auth/session/logout` (bearer, no body) -> `204`: sign this
    /// device's session out at the server (ADR-0074 section 2.1). Afterwards
    /// its bearer gets a plain `401` and its refresh `401 refresh-invalid`.
    public func logoutSession(sessionToken: String) async throws {
        guard !sessionToken.isEmpty else {
            throw AuthClientError.argument("sessionToken must be non-empty")
        }
        let url = try buildURL(endpoint: "/auth/session/logout")
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("Bearer \(sessionToken)", forHTTPHeaderField: "Authorization")
        try await executeNoContent(endpoint: "/auth/session/logout", request: request)
    }

    /// `POST /account/devices/revoke` (bearer) -> `204`: sign ANOTHER device
    /// of the account out at the server (ADR-0074 section 2.1). `statement`
    /// is the 138-byte `device-revoke` account statement over `deviceId`,
    /// signed right before this call (valid 120 s).
    ///
    /// Throws ``AuthClientError/http(status:endpoint:body:)``:
    /// `403 fresh-auth-required` (checked first), `400` (a bad id, this
    /// device's own id), `403 account-key-required` (no recorded account key,
    /// or the statement failed), `429 too-many-attempts`.
    public func revokeDevice(
        sessionToken: String,
        freshGrant: String,
        deviceId: Data,
        statement: Data
    ) async throws {
        guard !sessionToken.isEmpty, !freshGrant.isEmpty else {
            throw AuthClientError.argument("sessionToken and freshGrant must be non-empty")
        }
        guard deviceId.count == 32 else {
            throw AuthClientError.argument("deviceId must be 32 bytes")
        }
        let url = try buildURL(endpoint: "/account/devices/revoke")
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("Bearer \(sessionToken)", forHTTPHeaderField: "Authorization")
        do {
            request.httpBody = try encoder.encode(DeviceRevokeRequest(
                freshGrant: freshGrant,
                deviceIdB64: base64UrlEncodeNoPad(deviceId),
                statementB64: base64UrlEncodeNoPad(statement)))
        } catch {
            throw AuthClientError.argument("failed to encode /account/devices/revoke body: \(error)")
        }
        try await executeNoContent(endpoint: "/account/devices/revoke", request: request)
    }

    /// `POST /account/keys/reset/start` (bearer).
    public func keyResetStart(
        sessionToken: String,
        _ request: KeyResetStartRequest
    ) async throws -> KeyResetStartResponse {
        try await post(endpoint: "/account/keys/reset/start", body: request, bearer: sessionToken)
    }

    /// `POST /account/keys/reset/finish` (bearer).
    public func keyResetFinish(
        sessionToken: String,
        _ request: KeyResetFinishRequest
    ) async throws -> KeyResetFinishResponse {
        try await post(endpoint: "/account/keys/reset/finish", body: request, bearer: sessionToken)
    }

    /// Reset the signed-in account's keys (ADR-0073 section 2): register a
    /// recovery credential made from `seed`, the 64-byte BIP39 seed of the
    /// NEW phrase, and bind `newSignaturePubkey`, the NEW AccountIdentity
    /// key, to the account at the next key generation:
    ///
    /// ```text
    ///   client: OPAQUE recovery registration start (seed)
    ///   POST /account/keys/reset/start   { fresh_grant, recovery_request }  -> { recovery_response }
    ///   client: OPAQUE recovery registration finish
    ///   POST /account/keys/reset/finish  { fresh_grant, recovery_upload, new_signature_pubkey_b64 }
    ///                                    -> { key_reset_attest_b64, <session pair> }
    /// ```
    ///
    /// The finish is ONE server transaction: it bumps the generation,
    /// replaces the recovery credential, deletes every passkey, vault,
    /// action grant, device grant and device request of the account, mints
    /// the attestation, and issues the caller a session pair at the new
    /// generation (the caller's old pair is superseded with every other
    /// device's; install the returned one at once). `start` stores nothing,
    /// so a failure in either call changes nothing.
    ///
    /// The messages are built exactly as signup builds its recovery half.
    ///
    /// `freshGrant` is the grant a password login minted (amended
    /// 2026-10-04); one grant serves both calls. Without a valid one both
    /// answer `403 {"error":"fresh-auth-required"}` and nothing is done.
    ///
    /// Throws ``AuthClientError/http(status:endpoint:body:)`` (`401`, `403`, `400`,
    /// `429 too-many-attempts`, `503 rate-limiter-unavailable`) or
    /// ``AuthClientError/network(endpoint:underlying:)``.
    public func resetAccountKeys(
        sessionToken: String,
        freshGrant: String,
        seed: Data,
        newSignaturePubkey: Data
    ) async throws -> KeyResetResult {
        guard !sessionToken.isEmpty else {
            throw AuthClientError.argument("sessionToken must be non-empty")
        }
        guard !freshGrant.isEmpty else {
            throw AuthClientError.argument("freshGrant must be non-empty")
        }
        guard !seed.isEmpty else {
            throw AuthClientError.argument("seed must be non-empty")
        }
        guard newSignaturePubkey.count == 32 else {
            throw AuthClientError.argument("newSignaturePubkey must be 32 bytes (Ed25519)")
        }
        let reg = try OpaqueRegister.startRecovery(seed: seed)
        let start = try await keyResetStart(
            sessionToken: sessionToken,
            KeyResetStartRequest(freshGrant: freshGrant, recoveryRequest: reg.requestB64())
        )
        let upload = try reg.finish(
            secret: seed, serverResponseB64: start.recoveryResponse
        ).uploadB64()
        let finish = try await keyResetFinish(
            sessionToken: sessionToken,
            KeyResetFinishRequest(
                freshGrant: freshGrant,
                recoveryUpload: upload,
                newSignaturePubkeyB64: base64UrlEncodeNoPad(newSignaturePubkey)
            )
        )
        guard let attestation = base64UrlDecodeNoPad(finish.keyResetAttestB64),
              attestation.count == KeyResetResult.attestationLength else {
            throw AuthClientError.http(
                status: 200,
                endpoint: "/account/keys/reset/finish",
                body: "malformed key_reset_attest_b64 in response"
            )
        }
        return KeyResetResult(attestation: attestation, session: finish.sessionPair)
    }

    // MARK: - ADR-0074 part 3: rotating the account keys, the devices kept

    /// Rotate the signed-in account's keys (ADR-0074 section 3.4):
    ///
    /// ```text
    ///   POST /account/keys/rotate/start   { fresh_grant, recovery_request }  -> { recovery_response }
    ///   POST /account/keys/rotate/finish  { fresh_grant, recovery_upload, new_signature_pubkey_b64,
    ///                                       supersession_proof_b64, revoke_device_ids_b64[] }
    ///                                     -> { key_reset_attest_b64, <session pair> }
    /// ```
    ///
    /// Unlike a reset, auth VERIFIES `supersessionProof` (by the account key
    /// it has recorded, over `newSignaturePubkey`): a session that holds the
    /// password but not the account key cannot rotate. Passkeys, the
    /// password, TOTP and the other devices' session floors are kept; the
    /// passkey vaults are deleted. The answer carries no fresh-auth grant.
    ///
    /// Refusals, in the server's order: `403 fresh-auth-required`, `400`,
    /// `403 account-key-required`, `429 too-many-attempts`. `start` stores
    /// nothing, so a failure in either call changes nothing on the server.
    public func rotateAccountKeys(
        sessionToken: String,
        freshGrant: String,
        seed: Data,
        newSignaturePubkey: Data,
        supersessionProof: Data,
        revokeDeviceIds: [Data],
        phraseSaved: Bool = false
    ) async throws -> KeyResetResult {
        guard !sessionToken.isEmpty else {
            throw AuthClientError.argument("sessionToken must be non-empty")
        }
        guard !freshGrant.isEmpty else {
            throw AuthClientError.argument("freshGrant must be non-empty")
        }
        guard !seed.isEmpty else {
            throw AuthClientError.argument("seed must be non-empty")
        }
        guard newSignaturePubkey.count == 32 else {
            throw AuthClientError.argument("newSignaturePubkey must be 32 bytes (Ed25519)")
        }
        guard supersessionProof.count == KeyRotationWire.proofLength else {
            throw AuthClientError.argument("supersessionProof must be 145 bytes")
        }
        guard revokeDeviceIds.count <= KeyRotationWire.maxRevokedDevices else {
            throw AuthClientError.argument("at most 8 device ids may be revoked with a rotation")
        }
        let reg = try OpaqueRegister.startRecovery(seed: seed)
        let start: KeyResetStartResponse = try await post(
            endpoint: "/account/keys/rotate/start",
            body: KeyResetStartRequest(freshGrant: freshGrant, recoveryRequest: reg.requestB64()),
            bearer: sessionToken)
        let upload = try reg.finish(
            secret: seed, serverResponseB64: start.recoveryResponse
        ).uploadB64()
        let finish: KeyResetFinishResponse = try await post(
            endpoint: "/account/keys/rotate/finish",
            body: KeyRotateFinishRequest(
                freshGrant: freshGrant,
                recoveryUpload: upload,
                newSignaturePubkeyB64: base64UrlEncodeNoPad(newSignaturePubkey),
                supersessionProofB64: base64UrlEncodeNoPad(supersessionProof),
                revokeDeviceIdsB64: revokeDeviceIds.map(base64UrlEncodeNoPad),
                phraseSaved: phraseSaved ? true : nil),
            bearer: sessionToken)
        guard let attestation = base64UrlDecodeNoPad(finish.keyResetAttestB64),
              attestation.count == KeyResetResult.attestationLength else {
            throw AuthClientError.http(
                status: 200,
                endpoint: "/account/keys/rotate/finish",
                body: "malformed key_reset_attest_b64 in response"
            )
        }
        return KeyResetResult(attestation: attestation, session: finish.sessionPair)
    }

    /// The session upgrade of a KEPT device (ADR-0074 section 3.3): exchange
    /// `refreshToken`, a credential of the generation before a rotation, for
    /// a pair at the current generation naming the same device, by showing
    /// that this device holds the NEW account key (`newAccountEd25519Seed`).
    ///
    /// The statement is signed right before the request. A wrong clock is
    /// told the server's time (`401 statement-stale`) and the statement is
    /// signed again at it, once.
    ///
    /// Throws `401` with `key-generation-superseded` (the statement was not
    /// accepted: this key is not the account's), `device-revoked` (this
    /// device was the one removed), `refresh-invalid`, or `statement-stale`
    /// again; or ``AuthClientError/network(endpoint:underlying:)``.
    public func upgradeSession(
        refreshToken: String,
        accountId: Data,
        newAccountEd25519Seed: Data
    ) async throws -> SessionPair {
        guard !refreshToken.isEmpty else {
            throw AuthClientError.argument("refreshToken must be non-empty")
        }
        let response: SessionRefreshResponse = try await StatementStale.retrying { serverTimeMs in
            let statement = try serverTimeMs.map {
                try signAccountStatementAt(
                    accountSeed: newAccountEd25519Seed, kind: .sessionUpgrade, accountId: accountId,
                    body: sessionUpgradeStatementBody(refreshToken: refreshToken), unixMs: $0)
            } ?? signSessionUpgradeStatement(
                accountSeed: newAccountEd25519Seed, accountId: accountId, refreshToken: refreshToken)
            return try await self.post(
                endpoint: "/auth/session/refresh",
                body: SessionUpgradeRequest(
                    refreshToken: refreshToken, keyStatementB64: base64UrlEncodeNoPad(statement)))
        }
        return response.sessionPair
    }

    // MARK: - ADR-0074 part 1: taking the account back after a reset

    /// `POST /account/recall/status` (anonymous) — whether the account can
    /// still be taken back with the key `record` holds, until when, when the
    /// keys were reset, and how many password attempts are left. The
    /// statement is signed right before the request (it is valid 120 s) and
    /// never stored.
    ///
    /// `401 {"error":"recall-refused"}` means it cannot (any more).
    public func recallStatus(record: RecallRecord) async throws -> RecallWindow {
        guard let accountId = record.accountId, record.oldSeed.count == RecallRecord.seedLength else {
            throw AuthClientError.argument("the recall record is malformed")
        }
        // ADR-0074 "T5 as built" item 24 — a device with a wrong clock is
        // told the server's time (only its key's holder is) and signs again.
        let response: RecallStatusResponse = try await StatementStale.retrying { serverTimeMs in
            let statement = try serverTimeMs.map {
                try signAccountStatementAt(
                    accountSeed: record.oldSeed, kind: .recallStatus, accountId: accountId,
                    body: Data(), unixMs: $0)
            } ?? signAccountStatement(
                accountSeed: record.oldSeed, kind: .recallStatus, accountId: accountId, body: Data())
            return try await self.post(
                endpoint: "/account/recall/status",
                body: RecallStatusRequest(statementB64: base64UrlEncodeNoPad(statement)))
        }
        return response.window
    }

    /// `POST /account/recall/status` for a device that holds the recovery
    /// phrase and no account id (ADR-0075 section 8): the account is named
    /// by `handle`, and the statement, signed by the key derived from the
    /// typed phrase, carries the first 16 bytes of the handle where the
    /// account id goes. The answer carries the account id.
    ///
    /// `401 {"error":"recall-refused"}` is the one answer for an unknown
    /// name, words that were never this account's, and a passed window.
    public func recallStatusByHandle(handle: Data, oldEd25519Seed: Data) async throws -> RecallByHandle {
        try requireHandle(handle)
        guard oldEd25519Seed.count == RecallRecord.seedLength else {
            throw AuthClientError.argument("the account key seed must be 32 bytes")
        }
        let stated = Data(handle.prefix(accountIdLength))
        let wire = base64UrlEncodeNoPad(handle)
        let response: RecallStatusResponse = try await StatementStale.retrying { serverTimeMs in
            let statement = try serverTimeMs.map {
                try signAccountStatementAt(
                    accountSeed: oldEd25519Seed, kind: .recallStatus, accountId: stated,
                    body: Data(), unixMs: $0)
            } ?? signAccountStatement(
                accountSeed: oldEd25519Seed, kind: .recallStatus, accountId: stated, body: Data())
            return try await self.post(
                endpoint: "/account/recall/status",
                body: RecallStatusRequest(statementB64: base64UrlEncodeNoPad(statement), handle: wire))
        }
        guard let accountId = response.accountIdB64.flatMap(base64UrlDecodeNoPad),
              accountId.count == accountIdLength else {
            throw AuthClientError.http(
                status: 200, endpoint: "/account/recall/status", body: "malformed account_id_b64 in response")
        }
        return RecallByHandle(window: response.window, accountId: accountId)
    }

    /// Take the account back (ADR-0074 section 1.2, "T3 as built"):
    ///
    /// ```text
    ///   client: OPAQUE login start (old password), OPAQUE registration
    ///           starts (new password, new recovery seed), the
    ///           `recall-start` statement by the OLD account key
    ///   POST /account/recall/start   -> session_id, three responses, server_nonce
    ///   client: OPAQUE login finish. A failure here IS the wrong old
    ///           password: `RecallWrongPassword`, and `finish` is not called.
    ///   client: both registration finishes, the `recall` statement
    ///   POST /account/recall/finish  -> the reset's answer + a fresh-auth grant
    /// ```
    ///
    /// Each accepted `start` is one password attempt at the server. Throws
    /// ``RecallWrongPassword``, ``AuthClientError/http(status:endpoint:body:)``
    /// (`401 recall-refused`, `401 wrong-password`, `429`, `400`) or
    /// ``AuthClientError/network(endpoint:underlying:)``.
    public func recallAccount(_ attempt: RecallAttempt) async throws -> RecallLanded {
        guard let accountId = attempt.record.accountId,
              attempt.record.oldSeed.count == RecallRecord.seedLength else {
            throw AuthClientError.argument("the recall record is malformed")
        }
        // ADR-0075 section 8 — the proof is the old password, or the seed of
        // the recovery phrase as it was before the reset.
        let loginSecret = attempt.oldPhraseSeed ?? attempt.oldPassword
        guard !loginSecret.isEmpty, !attempt.newPassword.isEmpty else {
            throw AuthClientError.argument("both passwords must be non-empty")
        }
        guard attempt.newSignaturePubkey.count == 32 else {
            throw AuthClientError.argument("newSignaturePubkey must be 32 bytes (Ed25519)")
        }
        let oldSeed = attempt.record.oldSeed
        let accountIdB64 = base64UrlEncodeNoPad(accountId)

        let login = try attempt.oldPhraseSeed.map { try OpaqueLogin.startRecovery(seed: $0) }
            ?? OpaqueLogin.startPassword(password: attempt.oldPassword)
        let proof: String? = attempt.oldPhraseSeed == nil ? nil : "phrase"
        let passwordReg = try OpaqueRegister.startPassword(password: attempt.newPassword)
        let recoveryReg = try OpaqueRegister.startRecovery(seed: attempt.newSeed)
        let loginRequest = login.requestB64()
        let passwordRequest = passwordReg.requestB64()
        let recoveryRequest = recoveryReg.requestB64()
        // A stale statement was not counted as an attempt: the same OPAQUE
        // messages go again under a statement signed at the server's time.
        let start: RecallStartResponse = try await StatementStale.retrying { serverTimeMs in
            let startStatement = try serverTimeMs.map {
                try signAccountStatementAt(
                    accountSeed: oldSeed, kind: .recallStart, accountId: accountId,
                    body: try recallStartStatementBody(
                        anchorSeq: attempt.anchorSeq, loginRequestB64: loginRequest),
                    unixMs: $0)
            } ?? signRecallStartStatement(
                accountSeed: oldSeed, accountId: accountId, anchorSeq: attempt.anchorSeq,
                loginRequestB64: loginRequest)
            return try await self.post(
                endpoint: "/account/recall/start",
                body: RecallStartRequest(
                    accountIdB64: accountIdB64,
                    anchorSeq: attempt.anchorSeq,
                    statementB64: base64UrlEncodeNoPad(startStatement),
                    loginRequest: loginRequest,
                    proof: proof,
                    passwordRequest: passwordRequest,
                    recoveryRequest: recoveryRequest))
        }

        // The server's answer opens only under the password the account had
        // before the reset.
        let loginOut: OpaqueLoginOutput
        do {
            loginOut = try login.finish(secret: loginSecret, serverResponseB64: start.loginResponse)
        } catch {
            throw RecallWrongPassword()
        }
        guard let serverNonce = base64UrlDecodeNoPad(start.serverNonceB64) else {
            throw AuthClientError.http(
                status: 200, endpoint: "/account/recall/start", body: "malformed server_nonce_b64 in response")
        }
        let passwordUpload = try passwordReg.finish(
            secret: attempt.newPassword, serverResponseB64: start.passwordResponse
        ).uploadB64()
        let recoveryUpload = try recoveryReg.finish(
            secret: attempt.newSeed, serverResponseB64: start.recoveryResponse
        ).uploadB64()
        let deviceId = LandingDevice.usable(attempt.deviceId)
        let loginFinish = loginOut.finalizeB64()
        // "T5 as built" item 25 — the request is encoded ONCE per statement
        // and those exact bytes are sent again when no answer arrives: the
        // server answers a byte-identical `finish` again for ten minutes, with
        // the same attestation. The caller keeps the generated phrase until
        // this returns or throws an answer.
        let finish: RecallFinishResponse = try await StatementStale.retrying { serverTimeMs in
            let statement = try serverTimeMs.map {
                try signAccountStatementAt(
                    accountSeed: oldSeed, kind: .recall, accountId: accountId,
                    body: try recallStatementBody(
                        serverNonce: serverNonce, newSignaturePubkey: attempt.newSignaturePubkey,
                        passwordUploadB64: passwordUpload, recoveryUploadB64: recoveryUpload),
                    unixMs: $0)
            } ?? signRecallStatement(
                accountSeed: oldSeed, accountId: accountId, serverNonce: serverNonce,
                newSignaturePubkey: attempt.newSignaturePubkey,
                passwordUploadB64: passwordUpload, recoveryUploadB64: recoveryUpload)
            let body = try self.encodeBody(
                endpoint: "/account/recall/finish",
                RecallFinishRequest(
                    sessionId: start.sessionId,
                    accountIdB64: accountIdB64,
                    anchorSeq: attempt.anchorSeq,
                    loginFinish: loginFinish,
                    passwordUpload: passwordUpload,
                    recoveryUpload: recoveryUpload,
                    newSignaturePubkeyB64: base64UrlEncodeNoPad(attempt.newSignaturePubkey),
                    deviceIdB64: deviceId.map(base64UrlEncodeNoPad),
                    statementB64: base64UrlEncodeNoPad(statement)))
            return try await RecallFinishRetry.sendIdentical(body: body) { bytes in
                try await self.postRaw(endpoint: "/account/recall/finish", body: bytes)
            }
        }
        guard let attestation = base64UrlDecodeNoPad(finish.keyResetAttestB64),
              attestation.count == KeyResetResult.attestationLength else {
            throw AuthClientError.http(
                status: 200, endpoint: "/account/recall/finish",
                body: "malformed key_reset_attest_b64 in response")
        }
        return RecallLanded(
            reset: KeyResetResult(attestation: attestation, session: finish.sessionPair.naming(deviceId: deviceId)),
            freshGrant: FreshAuthGrant.from(grant: finish.freshGrant, expiresAtMs: finish.freshGrantExpiresAtMs))
    }

    // MARK: - The passkey vault: a passkey that restores keys

    /// `PUT /vault/passkey` — store the phrase entropy sealed under one of
    /// this account's passkeys, so that passkey can restore the keys on a
    /// new device (opt-in per passkey). `sealedEntropy` is the 44-byte blob
    /// of `passkeyVaultSeal`. Bearer-authenticated. `503` when the server
    /// has no vault key.
    public func passkeyVaultPut(
        sessionToken: String,
        credentialId: Data,
        sealedEntropy: Data
    ) async throws {
        guard !sessionToken.isEmpty else {
            throw AuthClientError.argument("sessionToken must be non-empty")
        }
        let _: PasskeyVaultPutResponse = try await send(
            method: "PUT",
            endpoint: "/vault/passkey",
            body: PasskeyVaultPutRequest(
                credentialIdB64: base64UrlEncodeNoPad(credentialId),
                sealedEntropyB64: base64Encode(sealedEntropy)
            ),
            bearer: sessionToken
        )
    }

    /// `POST /vault/passkey/get` — the blob stored for one credential, or
    /// `nil`. Bearer-authenticated.
    public func passkeyVaultGet(
        sessionToken: String,
        credentialId: Data
    ) async throws -> Data? {
        guard !sessionToken.isEmpty else {
            throw AuthClientError.argument("sessionToken must be non-empty")
        }
        let resp: PasskeyVaultGetResponse = try await post(
            endpoint: "/vault/passkey/get",
            body: PasskeyVaultCredentialRequest(
                credentialIdB64: base64UrlEncodeNoPad(credentialId)),
            bearer: sessionToken
        )
        guard let sealedB64 = resp.sealedEntropyB64 else { return nil }
        guard let sealed = base64Decode(sealedB64) else {
            throw AuthClientError.http(
                status: 200,
                endpoint: "/vault/passkey/get",
                body: "malformed sealed_entropy_b64 in response"
            )
        }
        return sealed
    }

    /// `DELETE /vault/passkey` — "Stop restoring keys": remove the blob
    /// stored for one credential. `credentialIdB64` is the base64url (no
    /// pad) id as `/webauthn/credentials` lists it. Returns whether a blob
    /// existed. Bearer-authenticated.
    @discardableResult
    public func passkeyVaultDelete(
        sessionToken: String,
        credentialIdB64: String
    ) async throws -> Bool {
        guard !sessionToken.isEmpty else {
            throw AuthClientError.argument("sessionToken must be non-empty")
        }
        let resp: PasskeyVaultDeleteResponse = try await send(
            method: "DELETE",
            endpoint: "/vault/passkey",
            body: PasskeyVaultCredentialRequest(credentialIdB64: credentialIdB64),
            bearer: sessionToken
        )
        return resp.deleted
    }

    /// `GET /vault/passkeys` — the credential ids (base64url, no pad) that
    /// have keys stored for them. Bearer-authenticated.
    public func passkeyVaultList(sessionToken: String) async throws -> [String] {
        guard !sessionToken.isEmpty else {
            throw AuthClientError.argument("sessionToken must be non-empty")
        }
        let url = try buildURL(endpoint: "/vault/passkeys")
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.setValue("Bearer \(sessionToken)", forHTTPHeaderField: "Authorization")
        let resp: PasskeyVaultListResponse = try await execute(
            endpoint: "/vault/passkeys", request: request)
        return resp.credentialIdsB64
    }

    // MARK: - Login handle binding (ADR-0054 S5)

    /// `PUT /auth/login-handle` - bind the signed-in account to `handle`,
    /// replacing its previous one. Idempotent for the handle it already holds.
    ///
    /// Throws ``AuthClientError/http(status:endpoint:body:)``: `409`
    /// (`handle-taken`, another account holds it; nothing changed), `503`
    /// (`login-by-name-disabled`), `400`, `401`.
    public func bindLoginHandle(handle: Data, sessionToken: String) async throws {
        try requireHandle(handle)
        guard !sessionToken.isEmpty else {
            throw AuthClientError.argument("sessionToken must be non-empty")
        }
        let _: LoginHandleBindResponse = try await send(
            method: "PUT",
            endpoint: "/auth/login-handle",
            body: LoginHandleBindRequest(handle: base64UrlEncodeNoPad(handle)),
            bearer: sessionToken
        )
    }

    /// `PUT /auth/login-handle` that CHANGES the account's handle (ADR-0076
    /// section 2): needs the fresh-auth grant and names the handle held now.
    /// With `recoveryEmail` the server re-keys the stored recovery email to
    /// the new handle (and replaces the vault when one is sent) in the same
    /// transaction; without it a change drops the recovery email.
    ///
    /// Throws `403 fresh-auth-required` / `address-mismatch`, `409
    /// handle-taken` / `handle-mismatch`, `429 handle-bind-limited`.
    public func changeLoginHandle(
        handle: Data, previousHandle: Data, freshGrant: String,
        recoveryEmail: LoginHandleRecoveryEmail?, sessionToken: String
    ) async throws {
        try requireHandle(handle)
        try requireHandle(previousHandle)
        let _: LoginHandleBindResponse = try await send(
            method: "PUT",
            endpoint: "/auth/login-handle",
            body: LoginHandleBindRequest(
                handle: base64UrlEncodeNoPad(handle), freshGrant: freshGrant,
                previousHandle: base64UrlEncodeNoPad(previousHandle), recoveryEmail: recoveryEmail),
            bearer: sessionToken
        )
    }

    /// `POST /auth/login-handle/check` - what the account's bound handle is
    /// relative to `handle` (``LoginHandleState``).
    public func checkLoginHandle(handle: Data, sessionToken: String) async throws -> LoginHandleState {
        try requireHandle(handle)
        let response: LoginHandleCheckResponse = try await post(
            endpoint: "/auth/login-handle/check",
            body: LoginHandleCheckRequest(handle: base64UrlEncodeNoPad(handle)),
            bearer: sessionToken)
        guard let state = LoginHandleState(rawValue: response.state) else {
            throw AuthClientError.http(
                status: 200, endpoint: "/auth/login-handle/check", body: "unknown state in response")
        }
        return state
    }

    /// `DELETE /auth/login-handle` - remove the signed-in account's handle.
    /// Returns whether one was removed.
    @discardableResult
    public func removeLoginHandle(sessionToken: String) async throws -> Bool {
        guard !sessionToken.isEmpty else {
            throw AuthClientError.argument("sessionToken must be non-empty")
        }
        let url = try buildURL(endpoint: "/auth/login-handle")
        var request = URLRequest(url: url)
        request.httpMethod = "DELETE"
        request.setValue("Bearer \(sessionToken)", forHTTPHeaderField: "Authorization")
        let resp: LoginHandleRemoveResponse = try await execute(
            endpoint: "/auth/login-handle", request: request)
        return resp.removed
    }

    // MARK: - Internal HTTP helpers

    func requireHandle(_ handle: Data) throws {
        guard handle.count == loginHandleLength else {
            throw AuthClientError.argument("handle must be \(loginHandleLength) bytes")
        }
    }

    /// The account id of a login / reset finish (standard base64, 16 bytes).
    private static func decodeAccountId(_ b64: String, endpoint: String) throws -> Data {
        guard let bytes = base64Decode(b64), bytes.count == accountIdLength else {
            throw AuthClientError.http(
                status: 200,
                endpoint: endpoint,
                body: "malformed account_id in response"
            )
        }
        return bytes
    }

    func post<Req: Encodable, Res: Decodable>(
        endpoint: String,
        body: Req,
        bearer: String? = nil
    ) async throws -> Res {
        try await send(method: "POST", endpoint: endpoint, body: body, bearer: bearer)
    }

    func send<Req: Encodable, Res: Decodable>(
        method: String,
        endpoint: String,
        body: Req,
        bearer: String? = nil
    ) async throws -> Res {
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = method
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

    /// Encode a request body once, so the exact bytes can be sent again.
    private func encodeBody<Req: Encodable>(endpoint: String, _ body: Req) throws -> Data {
        do {
            return try encoder.encode(body)
        } catch {
            throw AuthClientError.argument("failed to encode \(endpoint) body: \(error)")
        }
    }

    /// `POST` already-encoded JSON bytes, verbatim.
    private func postRaw<Res: Decodable>(
        endpoint: String,
        body: Data,
        bearer: String? = nil
    ) async throws -> Res {
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        if let bearer {
            request.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization")
        }
        request.httpBody = body
        return try await execute(endpoint: endpoint, request: request)
    }

    private func postNoBody<Res: Decodable>(
        endpoint: String,
        bearer: String? = nil
    ) async throws -> Res {
        let url = try buildURL(endpoint: endpoint)
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        if let bearer {
            request.setValue(
                "Bearer \(bearer)",
                forHTTPHeaderField: "Authorization"
            )
        }
        return try await execute(endpoint: endpoint, request: request)
    }

    func buildURL(endpoint: String) throws -> URL {
        guard let url = URL(string: baseURL + endpoint) else {
            throw AuthClientError.argument(
                "invalid baseURL '\(baseURL)' or endpoint '\(endpoint)'"
            )
        }
        return url
    }

    func execute<Res: Decodable>(
        endpoint: String,
        request: URLRequest
    ) async throws -> Res {
        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(request, session: session, config: .current)
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

    /// Send a request whose success carries no body (`204`).
    func executeNoContent(endpoint: String, request: URLRequest) async throws {
        let raw: (Data, URLResponse)
        do {
            raw = try await AntiCensorshipTransport.perform(request, session: session, config: .current)
        } catch {
            throw AuthClientError.network(endpoint: endpoint, underlying: error)
        }
        let (data, response) = raw
        guard let httpResponse = response as? HTTPURLResponse else {
            throw AuthClientError.http(status: 0, endpoint: endpoint, body: "non-HTTP response")
        }
        guard (200..<300).contains(httpResponse.statusCode) else {
            throw AuthClientError.http(
                status: httpResponse.statusCode,
                endpoint: endpoint,
                body: truncate(String(data: data, encoding: .utf8) ?? "<binary>"))
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
    /// The 12-word recovery phrase, present when signup was called
    /// with `enrolRecovery = true` (the default). The UI MUST
    /// display it exactly once and ask the user to record it
    /// somewhere safe — losing it means losing the only way back
    /// into the account if the password is forgotten.
    public let recoveryPhrase: String?
    /// The session pair the auth service issued (ADR-0073 section 3).
    public let session: SessionPair
    /// The fresh-auth grant the signup finish minted. Kept in memory only
    /// by the caller.
    public let freshGrant: FreshAuthGrant?

    public init(
        accountId: Data,
        exportKey: Data,
        recoveryPhrase: String? = nil,
        session: SessionPair,
        freshGrant: FreshAuthGrant? = nil
    ) {
        self.accountId = accountId
        self.exportKey = exportKey
        self.recoveryPhrase = recoveryPhrase
        self.session = session
        self.freshGrant = freshGrant
    }

    /// The access token of ``session``.
    public var sessionToken: String { session.sessionToken }
    /// The access token's expiry.
    public var sessionExpiresAtMs: Int64 { session.sessionExpiresAtMs }
}

/// Result of ``AuthClient/login(handle:password:)``.
public struct LoginResult: Equatable {
    public let accountId: Data
    /// Symmetric session key shared with the server. Use to bind a
    /// follow-up session token; **not** to encrypt user data.
    public let sessionKey: Data
    /// Same export key the user got at signup time — deterministic
    /// for the same password.
    public let exportKey: Data
    /// The session pair the auth service issued (ADR-0073 section 3).
    public let session: SessionPair
    /// The reset grant the password login minted (ADR-0073, amended
    /// 2026-10-04). Kept in memory only by the caller.
    public let freshGrant: FreshAuthGrant?

    public init(
        accountId: Data,
        sessionKey: Data,
        exportKey: Data,
        session: SessionPair,
        freshGrant: FreshAuthGrant? = nil
    ) {
        self.accountId = accountId
        self.sessionKey = sessionKey
        self.exportKey = exportKey
        self.session = session
        self.freshGrant = freshGrant
    }

    public var sessionToken: String { session.sessionToken }
    public var sessionExpiresAtMs: Int64 { session.sessionExpiresAtMs }
}

/// Result of ``AuthClient/recover(handle:mnemonicSeed:newPassword:)``.
public struct RecoverResult: Equatable {
    public let accountId: Data
    /// OPAQUE export key for the **new** password. Derive a fresh
    /// device root key — the previous device key (if any) is no
    /// longer reachable from the new password.
    public let exportKey: Data
    /// The fresh session pair for the rotated password.
    public let session: SessionPair

    public init(accountId: Data, exportKey: Data, session: SessionPair) {
        self.accountId = accountId
        self.exportKey = exportKey
        self.session = session
    }

    public var sessionToken: String { session.sessionToken }
    public var sessionExpiresAtMs: Int64 { session.sessionExpiresAtMs }
}

/// Result of ``AuthClient/deviceAuthorizeRedeem(grantTokenB64:)``
/// — the account id and the same session pair every other login flow
/// returns (ADR-0018 factor (a)).
public struct DeviceAuthorizeRedeemResult: Equatable {
    /// Raw 16-byte account id the grant belongs to - where a fresh device
    /// first learns it (ADR-0056).
    public let accountId: Data
    /// The session pair — the fresh device's only ACCESS on the no-password
    /// restore path.
    public let session: SessionPair

    public init(accountId: Data, session: SessionPair) {
        self.accountId = accountId
        self.session = session
    }

    public var sessionToken: String { session.sessionToken }
    public var sessionExpiresAtMs: Int64 { session.sessionExpiresAtMs }
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

/// Result of ``AuthClient/totpRecover(handle:code:)`` (ADR-0018
/// factor (c)) and ``AuthClient/recoveryLogin(handle:seed:)`` — the account
/// id and the same session pair every other login flow returns. ACCESS
/// only, no identity material.
public struct TotpRecoverResult: Equatable {
    /// Raw 16-byte account id the grant belongs to - where a fresh device
    /// first learns it (ADR-0056).
    public let accountId: Data
    public let session: SessionPair

    public init(accountId: Data, session: SessionPair) {
        self.accountId = accountId
        self.session = session
    }

    public var sessionToken: String { session.sessionToken }
    public var sessionExpiresAtMs: Int64 { session.sessionExpiresAtMs }
}

/// Result of ``AuthClient/resetAccountKeys(sessionToken:freshGrant:seed:newSignaturePubkey:)``
/// (ADR-0073 section 2).
public struct KeyResetResult: Equatable {
    /// The 177-byte `KeyResetAttestation`, verified by the client against the
    /// pinned attest key before it is carried to discovery and the DS.
    public static let attestationLength = 177

    /// The raw attestation bytes.
    public let attestation: Data
    /// The session pair at the new generation. Replaces the caller's pair at
    /// once: the old one is superseded.
    public let session: SessionPair

    public init(attestation: Data, session: SessionPair) {
        self.attestation = attestation
        self.session = session
    }
}

// MARK: - Wire shapes of the refresh and passkey-vault routes

/// `POST /auth/session/refresh` response body: the session pair alone.
private struct SessionRefreshResponse: Decodable, SessionPairCarrying {
    let sessionToken: String
    let sessionExpiresAtMs: Int64
    let refreshToken: String
    let refreshExpiresAtMs: Int64
    let keyGeneration: UInt64

    enum CodingKeys: String, CodingKey {
        case sessionToken = "session_token"
        case sessionExpiresAtMs = "session_expires_at_ms"
        case refreshToken = "refresh_token"
        case refreshExpiresAtMs = "refresh_expires_at_ms"
        case keyGeneration = "key_generation"
    }
}

private struct PasskeyVaultPutRequest: Encodable {
    let credentialIdB64: String
    let sealedEntropyB64: String
    enum CodingKeys: String, CodingKey {
        case credentialIdB64 = "credential_id_b64"
        case sealedEntropyB64 = "sealed_entropy_b64"
    }
}

/// `{"stored": true}`; nothing in it is load-bearing.
private struct PasskeyVaultPutResponse: Decodable {}

private struct PasskeyVaultCredentialRequest: Encodable {
    let credentialIdB64: String
    enum CodingKeys: String, CodingKey { case credentialIdB64 = "credential_id_b64" }
}

private struct PasskeyVaultGetResponse: Decodable {
    let sealedEntropyB64: String?
    enum CodingKeys: String, CodingKey { case sealedEntropyB64 = "sealed_entropy_b64" }
}

private struct PasskeyVaultDeleteResponse: Decodable {
    let deleted: Bool
}

private struct PasskeyVaultListResponse: Decodable {
    let credentialIdsB64: [String]
    enum CodingKeys: String, CodingKey { case credentialIdsB64 = "credential_ids_b64" }
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
