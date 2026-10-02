// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AuthenticationServices
import Foundation
import SwiftUI

/// `PasskeysViewModel` — drives ``PasskeysView``.
///
/// Mirror of `apps/web/src/auth/PasskeysSection.tsx`. Holds the list of
/// stored credentials, which of them restore keys, the in-flight nickname
/// input, and orchestrates what the UI surfaces:
///
///   * `refresh()`     — `GET /webauthn/credentials` + `GET /vault/passkeys`.
///   * `addPasskey()`  — register (``PasskeyAdder``), then the opt-in keys
///     choice when the platform can protect keys with the passkey. Adding a
///     passkey stores NOTHING for it automatically.
///   * `offerKeysChoice(for:)` / `finishKeysChoice(_:)` — "Turn on restoring
///     keys" from a passkey's row.
///   * `stopRestoringKeys(_:)` — `DELETE /vault/passkey`.
///   * `deleteCredential(_:)` — `DELETE /webauthn/credentials/:id` (the
///     server also deletes that passkey's blob).
@MainActor
public final class PasskeysViewModel: ObservableObject {

    @Published public private(set) var credentials: [CredentialSummary] = []
    /// The credential ids (base64url) that have keys stored for them; `nil`
    /// when not loaded or the server keeps no keys for passkeys.
    @Published public private(set) var withKeys: [String]?
    /// The passkey the keys choice is open for, or `nil`.
    @Published public private(set) var keysChoice: KeysChoiceTarget?

    /// What the keys choice needs to know about its passkey.
    public struct KeysChoiceTarget: Equatable {
        public let credentialIdB64: String
        public let nickname: String
        /// The registration's PRF output, when the choice follows an add.
        public let prfOutput: Data?
    }

    /// Whether this platform can ask a passkey for the secret that protects
    /// keys: the PRF API exists from iOS 18.
    public static var prfApiAvailable: Bool {
        if #available(iOS 18.0, *) { return true }
        return false
    }
    @Published public private(set) var isLoading: Bool = false
    @Published public private(set) var isBusy: Bool = false
    @Published public var nickname: String = ""
    @Published public private(set) var message: StatusMessage?
    /// A3 — the prepared data-export file, ready for a share sheet once set.
    @Published public private(set) var exportFileUrl: URL?

    /// Kept as ``UiText`` so a message on screen follows a live language
    /// switch (ADR-0061).
    public enum StatusMessage: Equatable {
        case ok(UiText)
        case error(UiText)
    }

    private let authRepository: AuthRepository
    /// After the server-side deletion: the local wipe + sign-out, which is
    /// ``AppServices/wipeAccountLocalData(accountIdB64:)`` in the app, the
    /// same path Forget device takes. Required, so no caller can end up with
    /// a partial wipe of its own. The argument is the deleted account.
    private let wipeAfterDeletion: (String) -> Void
    private let client: WebauthnClient
    private let registration: WebauthnRegistration
    private var refreshTask: Task<Void, Never>?

    public init(
        authRepository: AuthRepository,
        wipeAfterDeletion: @escaping (String) -> Void,
        client: WebauthnClient? = nil,
        registration: WebauthnRegistration? = nil
    ) {
        // Default values constructed inside the @MainActor-
        // isolated body rather than in default-argument
        // position. Swift 6 strict concurrency rejects
        // `init() = WebauthnRegistration()` as a default arg
        // because the @MainActor-isolated `init` of
        // WebauthnRegistration cannot be evaluated in the
        // nonisolated context the default-argument expression
        // compiles into.
        self.authRepository = authRepository
        self.wipeAfterDeletion = wipeAfterDeletion
        self.client = client ?? WebauthnClient()
        self.registration = registration ?? WebauthnRegistration()
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
            await refreshWithKeys()
        } catch let error as WebauthnClientError {
            // 401 is harmless on the first render after logout —
            // the publisher hasn't fired yet. Stay quiet.
            if case .http(401, _, _) = error {
                credentials = []
            } else {
                message = .error(Self.explain(error))
            }
        } catch {
            NSLog("[passkeys] WARN list failed: \(error)")
            message = .error(.key("settings.passkeys.error.generic"))
        }
    }

    /// Which passkeys restore keys. A failure (or a server without a vault
    /// key) reads as "none known": every passkey then shows "Signs in only".
    private func refreshWithKeys() async {
        guard let token = authRepository.identitySecrets?.sessionToken else {
            withKeys = nil
            return
        }
        withKeys = try? await AuthClient().passkeyVaultList(sessionToken: token)
    }

    /// The label of one passkey's row.
    func keysLabel(for cred: CredentialSummary) -> PasskeyVault.KeysLabel {
        PasskeyVault.keysLabel(credentialIdB64: cred.credential_id_b64, withKeys: withKeys)
    }

    // MARK: - Restoring keys

    /// The enrollment the keys choice runs.
    var keysEnrollment: PasskeyKeysEnrollment {
        .live(authRepository: authRepository, client: client)
    }

    /// "Turn on restoring keys" on a passkey's row.
    public func offerKeysChoice(for cred: CredentialSummary) {
        message = nil
        keysChoice = KeysChoiceTarget(
            credentialIdB64: cred.credential_id_b64, nickname: cred.nickname, prfOutput: nil)
    }

    /// The keys choice ended.
    func finishKeysChoice(_ result: KeysChoiceResult) async {
        keysChoice = nil
        if let key = result.messageKey {
            message = result == .stored ? .ok(.key(key)) : .error(.key(key))
        }
        await refreshWithKeys()
    }

    /// "Stop restoring keys": remove the blob stored for the passkey. The
    /// passkey keeps signing in; keys already restored on other devices
    /// stay there.
    public func stopRestoringKeys(_ cred: CredentialSummary) async {
        guard let token = authRepository.identitySecrets?.sessionToken else {
            message = .error(.key("common.error.session_not_ready"))
            return
        }
        isBusy = true
        message = nil
        defer { isBusy = false }
        do {
            try await AuthClient().passkeyVaultDelete(
                sessionToken: token, credentialIdB64: cred.credential_id_b64)
            message = .ok(.key("settings.passkeys.stop_restoring.done"))
            await refreshWithKeys()
        } catch {
            NSLog("[passkeys] WARN stop restoring keys failed")
            message = .error(.key("settings.passkeys.error.generic"))
        }
    }

    // MARK: - Add

    public func addPasskey(presentationAnchor: ASPresentationAnchor) async {
        rebindSession()
        let trimmed = nickname.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            message = .error(.key("settings.passkeys.error.name_required"))
            return
        }
        guard
            authRepository.identitySecrets != nil,
            case .session(let session) = authRepository.state
        else {
            message = .error(.key("common.error.session_not_ready"))
            return
        }
        // Best-effort: surface the username in the
        // authenticator's UI. The server never stores it.
        let userName = session.username
        isBusy = true
        message = nil
        defer { isBusy = false }
        do {
            let added = try await PasskeyAdder.add(
                client: client,
                registration: registration,
                userName: userName,
                nickname: trimmed,
                account: authRepository.identitySecrets.map {
                    (accountIdBytes: $0.accountId, accountIdB64: base64Encode($0.accountId))
                },
                presentationAnchor: presentationAnchor
            )
            switch added.outcome {
            case .offerKeysChoice:
                // The platform can protect keys with this passkey: the
                // person chooses. Nothing is stored until "Turn on".
                keysChoice = KeysChoiceTarget(
                    credentialIdB64: added.credentialIdB64,
                    nickname: trimmed,
                    prfOutput: added.prfOutput
                )
                message = .ok(.key("settings.passkeys.added", texts: ["name": trimmed]))
            case .signInOnly:
                message = .ok(.key("settings.passkeys.added_no_keys"))
            }
            nickname = ""
            await refresh()
        } catch let error as WebauthnRegistration.RegistrationError {
            switch error {
            case .cancelled:
                message = .error(.key("settings.passkeys.error.cancelled"))
            case .noCredential, .underlying:
                NSLog("[passkeys] WARN registration failed: \(error)")
                message = .error(.key("settings.passkeys.error.generic"))
            }
        } catch let error as WebauthnClientError {
            message = .error(Self.explain(error))
        } catch {
            NSLog("[passkeys] WARN registration failed: \(error)")
            message = .error(.key("settings.passkeys.error.generic"))
        }
    }

    // MARK: - Delete

    public func deleteCredential(_ cred: CredentialSummary) async {
        rebindSession()
        isBusy = true
        message = nil
        defer { isBusy = false }
        do {
            // G-13 (ADR-0014): require a fresh passkey gesture
            // cryptographically bound to THIS credential's removal
            // before deleting it. A throw (e.g. the user dismisses
            // the sheet) aborts the delete — fail-closed.
            let actionHashB64 = try actionHashForDeleteCredential(
                credentialIdB64: cred.credential_id_b64
            )
            _ = try await client.assertAction(actionHashB64: actionHashB64)
            try await client.deleteCredential(credentialIdB64: cred.credential_id_b64)
            message = .ok(.key("settings.passkeys.removed", texts: ["name": cred.nickname]))
            await refresh()
        } catch let error as WebauthnAssertion.AssertionError {
            switch error {
            case .cancelled:
                message = .error(.key("settings.passkeys.remove.error.cancelled"))
            case .noCredential, .underlying:
                NSLog("[passkeys] WARN removal failed: \(error)")
                message = .error(.key("settings.passkeys.error.generic"))
            }
        } catch let error as WebauthnClientError {
            message = .error(Self.explain(error))
        } catch {
            NSLog("[passkeys] WARN removal failed: \(error)")
            message = .error(.key("settings.passkeys.error.generic"))
        }
    }

    /// A3 (GDPR erasure) — delete the whole account across services + wipe this
    /// device. Auth `DELETE /account` (on a 403, the account has a passkey, so
    /// run the action ceremony bound to this account + retry once, fail-closed)
    /// -> best-effort discovery purge -> the local wipe + sign-out Forget
    /// device also runs (``wipeAfterDeletion``).
    public func deleteAccount() async {
        rebindSession()
        guard let secrets = authRepository.identitySecrets else {
            message = .error(.key("common.error.session_not_ready"))
            return
        }
        isBusy = true
        message = nil
        defer { isBusy = false }
        do {
            do {
                try await client.deleteAccount()
            } catch let error as WebauthnClientError {
                if case .http(let status, _, _) = error, status == 403 {
                    let actionHashB64 = actionHashForDeleteAccount(
                        accountId: secrets.accountId
                    )
                    _ = try await client.assertAction(actionHashB64: actionHashB64)
                    try await client.deleteAccount()
                } else {
                    throw error
                }
            }
            // Best-effort discovery purge — the auth tier is the critical
            // erasure; the session token is about to be invalidated anyway.
            do {
                let discovery = DiscoveryClient(sessionToken: secrets.sessionToken)
                try await discovery.deleteAccount()
            } catch {
                // best-effort
            }
            // The local wipe + sign-out Forget device runs: every store of
            // the account on this device, chats included.
            wipeAfterDeletion(base64Encode(secrets.accountId))
        } catch let error as WebauthnAssertion.AssertionError {
            switch error {
            case .cancelled:
                message = .error(.key("settings.account.delete.error.cancelled"))
            case .noCredential, .underlying:
                NSLog("[account] WARN deletion failed: \(error)")
                message = .error(.key("settings.account.delete.error.generic"))
            }
        } catch let error as WebauthnClientError {
            message = .error(Self.explain(error))
        } catch {
            NSLog("[account] WARN deletion failed: \(error)")
            message = .error(.key("settings.account.delete.error.generic"))
        }
    }

    /// A3 (GDPR portability) — fetch what the server holds about the account,
    /// wrap it in a portable JSON document, write it to a temp file, and expose
    /// the file URL for a share sheet. The E2EE content lives on-device and is
    /// exported via the encrypted Backup, not here.
    public func exportData() async {
        rebindSession()
        isBusy = true
        message = nil
        defer { isBusy = false }
        do {
            let server = try await client.exportAccount()
            let doc = ExportDoc(
                schema: "lakko/account-export/v1",
                exported_at: ISO8601DateFormatter().string(from: Date()),
                server: server,
                note: Localization.shared.t("settings.data.export_note")
            )
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
            let data = try encoder.encode(doc)
            let url = FileManager.default.temporaryDirectory
                .appendingPathComponent("lakko-account-export.json")
            try data.write(to: url, options: .atomic)
            exportFileUrl = url
            message = .ok(.key("settings.data.ready"))
        } catch let error as WebauthnClientError {
            message = .error(Self.explain(error))
        } catch {
            NSLog("[account] WARN export failed: \(error)")
            message = .error(.key("settings.data.error.generic"))
        }
    }

    private struct ExportDoc: Encodable {
        let schema: String
        let exported_at: String
        let server: AccountExport
        let note: String
    }

    // MARK: - Helpers

    /// The copy for a passkey-service failure (ADR-0061: never the server's
    /// or the core's prose; that goes to the log).
    private static func explain(_ error: WebauthnClientError) -> UiText {
        switch error {
        case .http(let status, _, let body):
            switch status {
            case 401:
                return .key("common.error.session_expired")
            case 503:
                return .key("auth.passkey.error.server_disabled")
            default:
                NSLog("[passkeys] WARN server error \(status): \(body)")
                return .key("settings.passkeys.error.server_status", texts: ["status": String(status)])
            }
        case .network:
            return .key("settings.passkeys.error.network")
        case .argument, .decode:
            NSLog("[passkeys] WARN failed: \(error)")
            return .key("settings.passkeys.error.generic")
        }
    }
}
