// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// `WebauthnClient` — fetch-based wrapper for the
/// `/webauthn/*` routes in `lakko-svc-auth` (Phase 2a2d).
///
/// Mirror of `apps/web/src/auth/webauthn-client.ts`. The HTTP
/// surface is identical; what differs is the platform-side
/// `navigator.credentials.create/get` substitute — on iOS the
/// authenticator is driven via `ASAuthorizationController` (lives
/// in ``WebauthnRegistration``).
///
/// ## Default base URL
///
/// `http://localhost:8081` — same loopback as
/// ``AuthClient.defaultBaseURL``. Override for physical devices
/// (LAN IP) and production (Cloudflare tunnel URL).
///
/// ## Surface
///
///   * ``listCredentials()`` / ``deleteCredential(credentialIdB64:)``
///     — bearer-auth CRUD on the user's passkey set.
///   * ``startRegistration(userName:nickname:)`` /
///     ``finishRegistration(stateId:nickname:credential:)`` —
///     the two halves of the registration round-trip; the
///     middle (`navigator.credentials.create` substitute) is
///     driven by ``WebauthnRegistration``.
///   * ``startAuthentication(accountIdB64:)`` /
///     ``finishAuthentication(stateId:credential:)`` — login
///     halves, driven end-to-end by ``loginWithPasskey()``.
///     Consumed by the UI: the Login screen's "Sign in with
///     passkey" button → ``AuthRepository/loginWithPasskey()``
///     (2a2c2, landed).
public final class WebauthnClient {

    public static let defaultBaseURL: String = "http://localhost:8081"

    public let baseURL: String
    private let session: URLSession
    /// Bearer token bound to the four authenticated routes
    /// (`/register/*`, `/credentials`, `/credentials/:id`). The
    /// anonymous `/auth/*` routes ignore it.
    private var sessionToken: String?

    public init(
        baseURL: String = WebauthnClient.defaultBaseURL,
        session: URLSession = .shared
    ) {
        self.baseURL = baseURL
        self.session = session
        self.sessionToken = nil
    }

    /// Bind / rebind the bearer token. Mirror of
    /// ``StorageClient/setSessionToken(_:)``. Caller drives from
    /// the same ``AuthRepository.identitySecrets?.sessionToken``
    /// signal every authed client consumes.
    public func setSessionToken(_ token: String?) {
        self.sessionToken = token
    }

    // MARK: - Registration

    public func startRegistration(
        userName: String,
        nickname: String
    ) async throws -> RegisterStartResponse {
        let body = RegisterStartRequest(user_name: userName, nickname: nickname)
        return try await postJson(
            endpoint: "/webauthn/register/start",
            body: body,
            authed: true
        )
    }

    public func finishRegistration(
        stateId: String,
        nickname: String,
        credential: RegisterPublicKeyCredentialJSON
    ) async throws -> RegisterFinishResponse {
        let body = RegisterFinishRequest(
            state_id: stateId,
            nickname: nickname,
            credential: credential
        )
        return try await postJson(
            endpoint: "/webauthn/register/finish",
            body: body,
            authed: true
        )
    }

    // MARK: - Authentication

    public func startAuthentication(
        accountIdB64: String?
    ) async throws -> AuthStartResponse {
        let body = AuthStartRequest(account_id_b64: accountIdB64)
        return try await postJson(
            endpoint: "/webauthn/auth/start",
            body: body,
            authed: false
        )
    }

    public func finishAuthentication(
        stateId: String,
        credential: AuthPublicKeyCredentialJSON
    ) async throws -> AuthFinishResponse {
        let body = AuthFinishRequest(state_id: stateId, credential: credential)
        return try await postJson(
            endpoint: "/webauthn/auth/finish",
            body: body,
            authed: false
        )
    }

    /// Phase 2a2c2-ios — drive the full passkey-login round in
    /// one call. Mirrors `WebauthnClient.loginWithPasskey` on
    /// the web side and exposes the same triple
    /// `(accountIdB64, sessionToken, sessionExpiresAtMs)` the
    /// OPAQUE login / recover paths surface.
    ///
    /// Discoverable-credential flow is the default
    /// (`account_id_b64` is omitted on /start); the
    /// authenticator picks any passkey it has for the lakko
    /// relying-party id. Caller MUST run on `@MainActor` so
    /// the embedded `ASAuthorizationController` can present
    /// the system sheet via ``currentPresentationAnchor``.
    @MainActor
    public func loginWithPasskey() async throws -> PasskeyLoginResult {
        let start = try await startAuthentication(accountIdB64: nil)
        let publicKey = start.options.publicKey
        let challenge = try base64UrlDecode(publicKey.challenge)
        guard let rpId = publicKey.rpId else {
            throw WebauthnClientError.argument(
                "server omitted rp_id in /webauthn/auth/start response"
            )
        }
        let allowedCredentials: [Data] = try (publicKey.allowCredentials ?? [])
            .map { try base64UrlDecode($0.id) }
        let credentialJSON = try await WebauthnAssertion().run(
            rpId: rpId,
            challenge: challenge,
            allowedCredentials: allowedCredentials,
            presentationAnchor: currentPresentationAnchor()
        )
        let finish = try await finishAuthentication(
            stateId: start.state_id,
            credential: credentialJSON
        )
        return PasskeyLoginResult(
            accountIdB64: finish.account_id_b64,
            sessionToken: finish.session_token,
            sessionExpiresAtMs: finish.session_expires_at_ms
        )
    }

    // MARK: - Credential management

    public func listCredentials() async throws -> ListCredentialsResponse {
        try await getJson(endpoint: "/webauthn/credentials", authed: true)
    }

    public func deleteCredential(credentialIdB64: String) async throws {
        let endpoint = "/webauthn/credentials/\(credentialIdB64)"
        var request = try buildRequest(endpoint: endpoint, method: "DELETE")
        attachAuth(to: &request)
        let (data, response) = try await performRequest(
            request,
            endpoint: endpoint
        )
        guard let httpResponse = response as? HTTPURLResponse else {
            throw WebauthnClientError.http(
                status: 0,
                endpoint: endpoint,
                body: "no HTTP response"
            )
        }
        switch httpResponse.statusCode {
        case 200, 204, 404:
            // 404 is idempotent — already deleted.
            return
        default:
            let body = String(data: data, encoding: .utf8) ?? ""
            throw WebauthnClientError.http(
                status: httpResponse.statusCode,
                endpoint: endpoint,
                body: Self.truncate(body)
            )
        }
    }

    // MARK: - Internal HTTP

    /// Bearer-authenticated JSON POST, reachable from same-module
    /// extensions (the action-binding ceremony in
    /// ``WebauthnActionBinding``). Thin wrapper over the private
    /// ``postJson(endpoint:body:authed:)`` so the transport,
    /// auth-header attach, and error mapping stay single-sourced.
    func postJsonAuthed<Req: Encodable, Res: Decodable>(
        endpoint: String,
        body: Req
    ) async throws -> Res {
        try await postJson(endpoint: endpoint, body: body, authed: true)
    }

    private func postJson<Req: Encodable, Res: Decodable>(
        endpoint: String,
        body: Req,
        authed: Bool
    ) async throws -> Res {
        var request = try buildRequest(endpoint: endpoint, method: "POST")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        if authed {
            attachAuth(to: &request)
        }
        let encoder = JSONEncoder()
        request.httpBody = try encoder.encode(body)
        let (data, response) = try await performRequest(request, endpoint: endpoint)
        return try Self.decode(data: data, response: response, endpoint: endpoint)
    }

    private func getJson<Res: Decodable>(
        endpoint: String,
        authed: Bool
    ) async throws -> Res {
        var request = try buildRequest(endpoint: endpoint, method: "GET")
        if authed {
            attachAuth(to: &request)
        }
        let (data, response) = try await performRequest(request, endpoint: endpoint)
        return try Self.decode(data: data, response: response, endpoint: endpoint)
    }

    private func performRequest(
        _ request: URLRequest,
        endpoint: String
    ) async throws -> (Data, URLResponse) {
        do {
            return try await session.data(for: request)
        } catch {
            throw WebauthnClientError.network(endpoint: endpoint, underlying: error)
        }
    }

    private func buildRequest(endpoint: String, method: String) throws -> URLRequest {
        guard let url = URL(string: baseURL + endpoint) else {
            throw WebauthnClientError.argument(
                "invalid baseURL '\(baseURL)' or endpoint '\(endpoint)'"
            )
        }
        var request = URLRequest(url: url)
        request.httpMethod = method
        return request
    }

    private func attachAuth(to request: inout URLRequest) {
        if let token = sessionToken {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
    }

    private static func decode<Res: Decodable>(
        data: Data,
        response: URLResponse,
        endpoint: String
    ) throws -> Res {
        guard let httpResponse = response as? HTTPURLResponse else {
            throw WebauthnClientError.http(
                status: 0,
                endpoint: endpoint,
                body: "no HTTP response"
            )
        }
        guard (200..<300).contains(httpResponse.statusCode) else {
            let body = String(data: data, encoding: .utf8) ?? ""
            throw WebauthnClientError.http(
                status: httpResponse.statusCode,
                endpoint: endpoint,
                body: truncate(body)
            )
        }
        let decoder = JSONDecoder()
        do {
            return try decoder.decode(Res.self, from: data)
        } catch {
            let body = String(data: data, encoding: .utf8) ?? ""
            throw WebauthnClientError.decode(
                endpoint: endpoint,
                body: truncate(body),
                underlying: error
            )
        }
    }

    private static func truncate(_ s: String, max: Int = 512) -> String {
        if s.count > max {
            return String(s.prefix(max)) + "…"
        }
        return s
    }
}

// MARK: - Wire shapes (mirror server-side `webauthn.rs`)

public struct RegisterStartRequest: Encodable {
    public let user_name: String
    public let nickname: String
}

public struct RegisterStartResponse: Decodable {
    public let state_id: String
    public let nickname: String
    public let options: ServerCreationOptions
}

/// `CreationChallengeResponse` from webauthn-rs. The outer
/// `publicKey` wraps the standard WebAuthn structure with
/// base64url-string slots in every BufferSource position.
public struct ServerCreationOptions: Decodable {
    public let publicKey: ServerPublicKeyCredentialCreationOptions
}

public struct ServerPublicKeyCredentialCreationOptions: Decodable {
    public let rp: RelyingParty
    public let user: User
    public let challenge: String
    public let pubKeyCredParams: [PubKeyCredParam]
    public let timeout: UInt32?
    public let excludeCredentials: [CredentialDescriptor]?
    public let authenticatorSelection: AuthenticatorSelection?
    public let attestation: String?

    public struct RelyingParty: Decodable {
        public let id: String
        public let name: String
    }

    public struct User: Decodable {
        public let id: String
        public let name: String
        public let displayName: String
    }

    public struct PubKeyCredParam: Decodable {
        public let type: String
        public let alg: Int
    }

    public struct CredentialDescriptor: Decodable {
        public let type: String
        public let id: String
        public let transports: [String]?
    }

    public struct AuthenticatorSelection: Decodable {
        public let authenticatorAttachment: String?
        public let residentKey: String?
        public let requireResidentKey: Bool?
        public let userVerification: String?
    }
}

public struct RegisterFinishRequest: Encodable {
    public let state_id: String
    public let nickname: String
    public let credential: RegisterPublicKeyCredentialJSON
}

/// What the server's `RegisterPublicKeyCredential` (webauthn-rs)
/// expects after the authenticator finishes. The iOS-side
/// `ASAuthorizationPlatformPublicKeyCredentialRegistration`
/// surfaces the same fields as raw `Data`; we base64url-encode
/// them in ``WebauthnRegistration``.
public struct RegisterPublicKeyCredentialJSON: Encodable {
    public let id: String
    public let rawId: String
    public let type: String
    public let response: Response
    public let extensions: [String: AnyEncodable]?

    public struct Response: Encodable {
        public let clientDataJSON: String
        public let attestationObject: String
        public let transports: [String]?
    }

    public init(
        id: String,
        rawId: String,
        response: Response,
        extensions: [String: AnyEncodable]? = nil
    ) {
        self.id = id
        self.rawId = rawId
        self.type = "public-key"
        self.response = response
        self.extensions = extensions
    }
}

public struct RegisterFinishResponse: Decodable {
    public let credential_id_b64: String
}

public struct AuthStartRequest: Encodable {
    public let account_id_b64: String?
}

public struct AuthStartResponse: Decodable {
    public let state_id: String
    public let discoverable: Bool
    public let options: ServerRequestOptions
}

public struct ServerRequestOptions: Decodable {
    public let publicKey: ServerPublicKeyCredentialRequestOptions
}

public struct ServerPublicKeyCredentialRequestOptions: Decodable {
    public let challenge: String
    public let timeout: UInt32?
    public let rpId: String?
    public let allowCredentials: [CredentialDescriptor]?
    public let userVerification: String?

    public struct CredentialDescriptor: Decodable {
        public let type: String
        public let id: String
        public let transports: [String]?
    }
}

public struct AuthFinishRequest: Encodable {
    public let state_id: String
    public let credential: AuthPublicKeyCredentialJSON
}

public struct AuthPublicKeyCredentialJSON: Encodable {
    public let id: String
    public let rawId: String
    public let type: String
    public let response: Response
    public let extensions: [String: AnyEncodable]?

    public struct Response: Encodable {
        public let clientDataJSON: String
        public let authenticatorData: String
        public let signature: String
        public let userHandle: String?
    }

    public init(
        id: String,
        rawId: String,
        response: Response,
        extensions: [String: AnyEncodable]? = nil
    ) {
        self.id = id
        self.rawId = rawId
        self.type = "public-key"
        self.response = response
        self.extensions = extensions
    }
}

public struct AuthFinishResponse: Decodable {
    public let account_id_b64: String
    public let session_token: String
    public let session_expires_at_ms: Int64
}

/// Phase 2a2c2-ios — high-level result of
/// ``WebauthnClient/loginWithPasskey()``. Mirror of the OPAQUE
/// login result shape, minus the export key (which on iOS lives
/// in ``ExportKeyStore`` and is looked up by the
/// ``AuthRepository`` caller after the assertion completes).
public struct PasskeyLoginResult {
    public let accountIdB64: String
    public let sessionToken: String
    public let sessionExpiresAtMs: Int64
}

public struct ListCredentialsResponse: Decodable {
    public let credentials: [CredentialSummary]
}

public struct CredentialSummary: Decodable, Identifiable {
    public let credential_id_b64: String
    public let nickname: String
    public let created_at_ms: Int64?
    public let last_used_at_ms: Int64?

    public var id: String { credential_id_b64 }
}

/// Type-erased encodable for `extensions` JSON objects. WebAuthn
/// extension payloads vary by extension; the server treats them
/// as opaque JSON. iOS today does not surface any client
/// extension results we care about, so this slot is empty in
/// practice — kept for parity with the wire shape.
public struct AnyEncodable: Encodable {
    private let encode: (Encoder) throws -> Void
    public init<T: Encodable>(_ value: T) {
        self.encode = value.encode
    }
    public func encode(to encoder: Encoder) throws {
        try encode(encoder)
    }
}

// MARK: - Errors

public enum WebauthnClientError: Error, LocalizedError {
    case argument(String)
    case http(status: Int, endpoint: String, body: String)
    case network(endpoint: String, underlying: Error)
    case decode(endpoint: String, body: String, underlying: Error)

    public var errorDescription: String? {
        switch self {
        case .argument(let msg):
            return msg
        case .http(let status, let endpoint, let body):
            return "webauthn \(endpoint) → \(status): \(body)"
        case .network(let endpoint, let underlying):
            return "webauthn \(endpoint) network error: \(underlying.localizedDescription)"
        case .decode(let endpoint, let body, let underlying):
            return "webauthn \(endpoint) decode error: \(underlying.localizedDescription) "
                + "(body: \(body))"
        }
    }
}
