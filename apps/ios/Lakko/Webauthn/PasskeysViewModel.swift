// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AuthenticationServices
import Foundation
import SwiftUI

/// `PasskeysViewModel` — drives ``PasskeysView`` (Phase 2a2d).
///
/// Mirror of `apps/web/src/auth/PasskeysSection.tsx`. Holds the
/// list of stored credentials + the in-flight nickname input and
/// orchestrates the three operations the UI surfaces:
///
///   * `refresh()`     — `GET /webauthn/credentials`.
///   * `addPasskey()`  — `POST /register/start` →
///     ``WebauthnRegistration.run`` → `POST /register/finish`.
///   * `deleteCredential(_:)` — `DELETE /webauthn/credentials/:id`.
///
/// Sign-in-with-passkey is deferred to 2a2c2 (see the web-side
/// notes — without a local exportKey cache, a passkey-only
/// session can't reconstruct Identity / device-key / MLS
/// bootstrap).
@MainActor
public final class PasskeysViewModel: ObservableObject {

    @Published public private(set) var credentials: [CredentialSummary] = []
    @Published public private(set) var isLoading: Bool = false
    @Published public private(set) var isBusy: Bool = false
    @Published public var nickname: String = ""
    @Published public private(set) var message: StatusMessage?

    public enum StatusMessage: Equatable {
        case ok(String)
        case error(String)
    }

    private let authRepository: AuthRepository
    private let client: WebauthnClient
    private let registration: WebauthnRegistration
    private var refreshTask: Task<Void, Never>?

    public init(
        authRepository: AuthRepository,
        client: WebauthnClient = WebauthnClient(),
        registration: WebauthnRegistration = WebauthnRegistration()
    ) {
        self.authRepository = authRepository
        self.client = client
        self.registration = registration
        rebindSession()
    }

    /// Re-bind the session token from the current ``AuthRepository``
    /// state. Called on init + on any session-flip (login,
    /// logout, recover). Idempotent.
    public func rebindSession() {
        client.setSessionToken(authRepository.identitySecrets?.sessionToken)
    }

    // MARK: - List

    public func refresh() async {
        rebindSession()
        guard authRepository.identitySecrets != nil else {
            credentials = []
            return
        }
        isLoading = true
        defer { isLoading = false }
        do {
            let response = try await client.listCredentials()
            credentials = response.credentials
        } catch let error as WebauthnClientError {
            // 401 is harmless on the first render after logout —
            // the publisher hasn't fired yet. Stay quiet.
            if case .http(401, _, _) = error {
                credentials = []
            } else {
                message = .error(Self.explain(error))
            }
        } catch {
            message = .error(error.localizedDescription)
        }
    }

    // MARK: - Add

    public func addPasskey(presentationAnchor: ASPresentationAnchor) async {
        rebindSession()
        let trimmed = nickname.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            message = .error("Give the passkey a nickname.")
            return
        }
        guard
            authRepository.identitySecrets != nil,
            case .session(let session) = authRepository.state
        else {
            message = .error("Session not ready.")
            return
        }
        // Best-effort: surface the username in the
        // authenticator's UI. The server never stores it.
        let userName = session.username
        isBusy = true
        message = nil
        defer { isBusy = false }
        do {
            let start = try await client.startRegistration(
                userName: userName,
                nickname: trimmed
            )
            let challenge = try base64UrlDecode(start.options.publicKey.challenge)
            let userId = try base64UrlDecode(start.options.publicKey.user.id)
            let credentialJSON = try await registration.run(
                rpId: start.options.publicKey.rp.id,
                challenge: challenge,
                userName: start.options.publicKey.user.name,
                userID: userId,
                presentationAnchor: presentationAnchor
            )
            _ = try await client.finishRegistration(
                stateId: start.state_id,
                nickname: start.nickname,
                credential: credentialJSON
            )
            message = .ok("Passkey \"\(trimmed)\" added.")
            nickname = ""
            await refresh()
        } catch let error as WebauthnRegistration.RegistrationError {
            switch error {
            case .cancelled:
                message = .error("Cancelled. No passkey was created.")
            case .noCredential:
                message = .error("Authenticator did not return a credential.")
            case .underlying(let underlying):
                message = .error(underlying.localizedDescription)
            }
        } catch let error as WebauthnClientError {
            message = .error(Self.explain(error))
        } catch {
            message = .error(error.localizedDescription)
        }
    }

    // MARK: - Delete

    public func deleteCredential(_ cred: CredentialSummary) async {
        rebindSession()
        isBusy = true
        message = nil
        defer { isBusy = false }
        do {
            try await client.deleteCredential(credentialIdB64: cred.credential_id_b64)
            message = .ok("Removed \"\(cred.nickname)\".")
            await refresh()
        } catch let error as WebauthnClientError {
            message = .error(Self.explain(error))
        } catch {
            message = .error(error.localizedDescription)
        }
    }

    // MARK: - Helpers

    private static func explain(_ error: WebauthnClientError) -> String {
        switch error {
        case .argument(let msg):
            return msg
        case .http(let status, _, let body):
            switch status {
            case 401:
                return "Session expired. Sign in again."
            case 503:
                return "Server hasn't enabled passkey support."
            default:
                return body.isEmpty
                    ? "Server error (\(status))."
                    : "Server error (\(status)): \(body)"
            }
        case .network:
            return "Network error. Check your connection and retry."
        case .decode:
            return "Could not parse server response."
        }
    }
}
