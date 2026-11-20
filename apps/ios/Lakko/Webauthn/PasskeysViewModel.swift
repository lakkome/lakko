// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AuthenticationServices
import Combine
import Foundation
import SwiftUI

/// `PasskeysViewModel` — drives ``PasskeysView``.
///
/// Mirror of `apps/web/src/auth/PasskeysSection.tsx`. Holds the list of
/// stored credentials, which of them restore keys, the in-flight nickname
/// input, and orchestrates what the UI surfaces:
///
///   * `refresh()`     — `GET /webauthn/credentials` + `GET /vault/passkeys`.
///   * `addPasskey()`  — register (``PasskeyAdder``), then store the keys for
///     the new passkey with nothing to choose, wherever the platform can
///     carry them (UX `docs/ux/identity-recovery.md` 10.2).
///   * `restoreKeys(for:)` / `submitRepairPhrase()` — "Restore keys with this
///     passkey" on the row of a passkey that holds no keys: a repair, not a
///     setting.
///   * `deleteCredential(_:)` — `DELETE /webauthn/credentials/:id` (the
///     server also deletes that passkey's blob).
@MainActor
public final class PasskeysViewModel: ObservableObject {

    @Published public private(set) var credentials: [CredentialSummary] = []
    /// The credential ids (base64url) that have keys stored for them; `nil`
    /// when not loaded or the server keeps no keys for passkeys.
    @Published public private(set) var withKeys: [String]?
    /// The passkeys this device found cannot carry keys (base64url
    /// credential ids, ``PasskeyNoPrfStore``): their rows offer no repair.
    @Published public private(set) var knownNoPrf: Set<String> = []
    /// The passkey "Restore keys with this passkey" is asking the recovery
    /// phrase for, or `nil`.
    @Published public private(set) var repairAskingPhrase: String?
    /// The phrase typed for the repair.
    @Published public var repairPhrase: String = ""

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

    /// The proof block above "Add a passkey" (UX 17.1): absent while a
    /// fresh-auth proof is live.
    let addProof: FreshAuthProofModel
    private var addProofSink: AnyCancellable?

    /// UX 15.3 — the keys were reset on this device in this session, so the
    /// empty state explains where the passkeys went.
    public var keysResetThisSession: Bool { authRepository.keysResetThisSession }
    /// After the server-side deletion: the local wipe + sign-out, which is
    /// ``AppServices/wipeAccountLocalData(accountIdB64:)`` in the app, the
    /// same path Forget device takes. Required, so no caller can end up with
    /// a partial wipe of its own. The argument is the deleted account.
    private let wipeAfterDeletion: (String) -> Void
    /// ADR-0098 — post the account-deleted statement in every conversation
    /// this device holds; ``ChatRepository/announceAccountDeleted()`` in the
    /// app. Run as one of the cleanups, after the auth delete and before the
    /// wipe. `nil` posts nothing.
    private let announceDeletion: (@MainActor () async throws -> Void)?
    private let client: WebauthnClient
    private let registration: WebauthnRegistration
    /// The ADR-0014 gesture bound to deleting the account (one system
    /// passkey sheet); injectable so a test can count it.
    private let assertDeleteAction: @MainActor (_ actionHashB64: String) async throws -> Void
    private var refreshTask: Task<Void, Never>?

    public init(
        authRepository: AuthRepository,
        wipeAfterDeletion: @escaping (String) -> Void,
        announceDeletion: (@MainActor () async throws -> Void)? = nil,
        client: WebauthnClient? = nil,
        registration: WebauthnRegistration? = nil,
        assertDeleteAction: (@MainActor (_ actionHashB64: String) async throws -> Void)? = nil
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
        self.addProof = FreshAuthProofModel(surface: .addPasskey, repository: authRepository)
        self.wipeAfterDeletion = wipeAfterDeletion
        self.announceDeletion = announceDeletion
        let client = client ?? WebauthnClient()
        self.client = client
        self.assertDeleteAction = assertDeleteAction ?? { hash in
            _ = try await client.assertAction(actionHashB64: hash)
        }
        self.registration = registration ?? WebauthnRegistration()
        rebindSession()
        // The block's fields decide whether "Add" is enabled.
        addProofSink = addProof.objectWillChange.sink { [weak self] _ in
            self?.objectWillChange.send()
        }
    }

    /// Re-bind the session token from the current ``AuthRepository``
    /// state. Called on init + on any session-flip (login,
    /// logout, recover). Idempotent.
    public func rebindSession() {
        // ADR-0099 step 3 — a suspended account signed in without its keys
        // deletes itself with the `s1`.
        client.setSessionToken(
            authRepository.identitySecrets?.sessionToken ?? authRepository.accountForDeletion()?.sessionToken)
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
            reloadKnownNoPrf()
            await refreshWithKeys()
        } catch let error where Cancellation.endedByCancellation(error) {
            // The screen was left while the list was loading: nothing failed,
            // and the next visit loads it again.
            return
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

    /// Read what this device remembers about passkeys that cannot carry keys.
    private func reloadKnownNoPrf() {
        guard case .session(let account) = authRepository.state else {
            knownNoPrf = []
            return
        }
        knownNoPrf = PasskeyNoPrfStore().load(accountIdB64: account.accountIdB64)
    }

    /// The label of one passkey's row.
    func keysLabel(for cred: CredentialSummary) -> PasskeyVault.KeysLabel {
        PasskeyVault.keysLabel(credentialIdB64: cred.credential_id_b64, withKeys: withKeys)
    }

    // MARK: - Restoring keys

    /// The enrollment storing the keys runs.
    var keysEnrollment: PasskeyKeysEnrollment {
        .live(authRepository: authRepository, client: client)
    }

    /// Whether a passkey's row offers "Restore keys with this passkey": it
    /// holds no keys, this platform can ask a passkey for the secret, and it
    /// is not known to give none.
    func offersRestoreKeys(for cred: CredentialSummary) -> Bool {
        keysLabel(for: cred) == .signInOnly
            && Self.prfApiAvailable
            && !knownNoPrf.contains(cred.credential_id_b64)
    }

    /// "Restore keys with this passkey" on a passkey's row. One passkey
    /// sheet; the recovery phrase is asked for first when this device does
    /// not hold it.
    public func restoreKeys(for cred: CredentialSummary) async {
        if repairAskingPhrase != cred.credential_id_b64 {
            repairAskingPhrase = nil
            repairPhrase = ""
        }
        await runRepair(credentialIdB64: cred.credential_id_b64, typedPhrase: nil)
    }

    /// The phrase typed for the repair was submitted.
    public func submitRepairPhrase() async {
        guard let credentialIdB64 = repairAskingPhrase else { return }
        await runRepair(credentialIdB64: credentialIdB64, typedPhrase: repairPhrase)
    }

    /// The phrase input of the repair was closed: nothing is stored.
    public func cancelRepair() {
        repairAskingPhrase = nil
        repairPhrase = ""
    }

    private func runRepair(credentialIdB64: String, typedPhrase: String?) async {
        rebindSession()
        isBusy = true
        message = nil
        defer { isBusy = false }
        do {
            let step = try await keysEnrollment.turnOn(
                credentialIdB64: credentialIdB64,
                registrationPrfOutput: nil,
                typedPhrase: typedPhrase
            )
            switch step {
            case .needsPhrase:
                repairAskingPhrase = credentialIdB64
            case .refused(let key):
                message = .error(.key(PasskeyKeysRepair.refusalKey(key)))
            case .done(let result):
                cancelRepair()
                reloadKnownNoPrf()
                switch result {
                case .stored:
                    message = .ok(.key("settings.passkeys.keys_choice.done"))
                case .noPrf:
                    message = .error(.key("settings.passkeys.keys_choice.no_prf"))
                case .unavailable:
                    message = .error(.key("settings.passkeys.keys_choice.unavailable"))
                }
                await refreshWithKeys()
            }
        } catch {
            // Dismissing the passkey sheet is not a failure: the passkey
            // stays "Logs in only" and its row still offers the repair.
            if AuthErrorCopy.isPasskeyCancelled(error) { return }
            if PasskeyKeysAddOutcome.isOffline(error) {
                message = .error(.key("settings.passkeys.error.network"))
            } else {
                NSLog("[passkeys] WARN restoring keys with a passkey failed")
                message = .error(.key("settings.passkeys.error.generic"))
            }
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
        // The fresh-auth proof first (ADR-0073, amended 2026-10-06): both
        // register calls carry it. With none live the block is on screen
        // and its password is checked now; a failure stays in the block.
        guard await addProof.ensure(), let freshGrant = authRepository.liveFreshGrant() else {
            if addProof.proofLive { addProof.proofExpired() }
            return
        }
        rebindSession()
        do {
            let added = try await PasskeyAdder.add(
                client: client,
                registration: registration,
                userName: userName,
                nickname: trimmed,
                account: authRepository.identitySecrets.map {
                    (accountIdBytes: $0.accountId, accountIdB64: base64Encode($0.accountId))
                },
                freshGrant: freshGrant,
                presentationAnchor: presentationAnchor
            )
            nickname = ""
            cancelRepair()
            // The passkey is added. Its keys are stored with nothing to
            // choose, from the registration's output or one more sheet for
            // this passkey (UX 10.2).
            let outcome = await keysEnrollment.storeAfterAdd(added)
            let line = outcome.message(nickname: trimmed)
            message = outcome.isError ? .error(line) : .ok(line)
            await refresh()
        } catch let error as WebauthnRegistration.RegistrationError {
            switch error {
            case .cancelled:
                message = .error(.key("settings.passkeys.error.cancelled"))
            case .alreadyExists:
                message = .error(.key("settings.passkeys.error.already_exists"))
            case .noCredential, .underlying:
                NSLog("[passkeys] WARN registration failed: \(error)")
                message = .error(.key("settings.passkeys.error.generic"))
            }
        } catch let error as WebauthnClientError {
            if FreshAuthRefusal.isRequired(error) {
                // The proof ran out between the two calls: the block
                // appears, with no error styling. Nothing was added.
                authRepository.dropFreshAuth()
                addProof.proofExpired()
            } else {
                message = .error(Self.explain(error))
            }
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
    /// device (UX 17.3).
    ///
    /// The proof `DELETE /account` needs depends on the account:
    ///
    ///   * WITH a passkey: the ADR-0014 gesture bound to this deletion is
    ///     itself the proof. That is the ONE system passkey sheet of the
    ///     flow; no passkey sign-in runs first and no fresh-auth grant is
    ///     obtained for it. A grant that happens to be live rides in the
    ///     header, and is not required.
    ///   * WITHOUT a passkey: a live fresh-auth grant, proven with the
    ///     password typed into the dialog's block (`proof`).
    ///
    /// Then auth `DELETE /account` -> best-effort discovery purge -> the
    /// local wipe + sign-out Forget device also runs (``wipeAfterDeletion``).
    /// Returns `false` when nothing was deleted; the dialog stays open and
    /// the block or ``message`` says why.
    @discardableResult
    func deleteAccount(proof: FreshAuthProofModel, hasPasskey: Bool) async -> Bool {
        rebindSession()
        // ADR-0099 step 3 — the signed-in account, or a suspended one signed
        // in here without its keys (its `s1`).
        guard let secrets = authRepository.accountForDeletion() else {
            message = .error(.key("common.error.session_not_ready"))
            return false
        }
        isBusy = true
        message = nil
        defer { isBusy = false }
        // 1. An account without a passkey proves with its password. Nothing
        //    is sent for the deletion before that passes.
        if !hasPasskey {
            guard await proof.ensure() else { return false }
            rebindSession()
            guard authRepository.liveFreshGrant() != nil else {
                proof.proofExpired()
                return false
            }
        }
        do {
            let actionHashB64 = actionHashForDeleteAccount(accountId: secrets.accountId)
            // 2. A passkey account: the gesture for THIS deletion (ADR-0014),
            //    the only passkey sheet of the flow.
            if hasPasskey {
                try await assertDeleteAction(actionHashB64)
            }
            // ADR-0078 section 10: the invite ledgers name no owner, so the
            // directory cannot delete this account's links. Revoke every live
            // link this device keeps a record of first, while the session
            // still works; one it has no record of lives until it expires.
            // ADR-0064 section 1 — the vaults of the channels this account
            // owns go before the account, best effort and bounded in time.
            await ChannelVaultHead.deleteOwnedVaults(
                accountIdB64: base64Encode(secrets.accountId),
                // A suspended account signed in without its keys holds no seed.
                currentSeed: authRepository.identitySecrets?.accountIdentitySeed)
            await OwnLinks.revokeAll(
                accountIdB64: base64Encode(secrets.accountId),
                discovery: DiscoveryClient(
                    sessionToken: authRepository.accountForDeletion()?.sessionToken ?? secrets.sessionToken))
            let accountIdB64 = base64Encode(secrets.accountId)
            // ADR-0099 step 3 — the session of the account being deleted: a
            // suspended account's `s1`, which discovery's account delete and
            // storage's backup delete accept.
            let token = authRepository.accountForDeletion()?.sessionToken ?? secrets.sessionToken
            // `POST /push/unsubscribe` takes no session, but each one spends
            // a `push-register` token, which auth issues only to a live
            // session: the run's `prepare` tops them up to one per
            // subscription before the auth delete. The ids are read now (the
            // wipe clears the route rows).
            let subscriptionIds = PushRoutingStore().subscriptionIds(accountIdB64: accountIdB64)
            let pushTokens = ReservedAnonTokens([])
            let hasPasskey = hasPasskey
            let client = self.client
            let authRepository = self.authRepository
            let assertDeleteAction = self.assertDeleteAction
            let wipeAfterDeletion = self.wipeAfterDeletion
            // Auth first; only once it accepted, the cleanups only this
            // device can do (discovery and storage verify the session token
            // statelessly, so it still works for them after the auth row is
            // gone, within its lifetime), then the local wipe + sign-out
            // Forget device also runs: every store of the account on this
            // device, chats included.
            let allCleanups = Self.deletionCleanups(
                sessionToken: token,
                subscriptionIds: subscriptionIds,
                pushTokens: pushTokens,
                announceDeletion: announceDeletion)
            try await AccountDeletionRun(
                prepare: {
                    await pushTokens.fill(
                        .pushRegister,
                        count: ReservedAnonTokens.countForDeletion(subscriptions: subscriptionIds.count),
                        takeAvailable: { AnonTokenPool.shared.takeIfAvailable(.pushRegister) },
                        issuer: AuthAnonTokenIssuer(),
                        sessionToken: token)
                },
                deleteAtAuth: {
                    do {
                        try await client.deleteAccount(freshGrant: authRepository.liveFreshGrant())
                    } catch let error as WebauthnClientError {
                        // A `403` that is not the fresh-auth refusal: the
                        // account has a passkey this device did not know of.
                        // Make the gesture and retry once, fail-closed.
                        if case .http(403, _, _) = error, !FreshAuthRefusal.isRequired(error), !hasPasskey {
                            try await assertDeleteAction(actionHashB64)
                            try await client.deleteAccount(freshGrant: authRepository.liveFreshGrant())
                        } else {
                            throw error
                        }
                    }
                },
                // ADR-0098 — the session refuses from here on; no cleanup's
                // 401 may sign this device out before the conversations are
                // told.
                afterAuth: { SessionTokenProvider.shared.markAccountDeleted() },
                // ADR-0098 (walk 10): the statement runs to the end before the
                // directory entry is removed.
                first: allCleanups.filter { $0.name == "conversations" },
                cleanups: allCleanups.filter { $0.name != "conversations" },
                wipe: { wipeAfterDeletion(accountIdB64) }
            ).run()
            return true
        } catch let error as WebauthnAssertion.AssertionError {
            switch error {
            case .cancelled:
                message = .error(.key("settings.account.delete.error.cancelled"))
            case .noCredential, .underlying:
                NSLog("[account] WARN deletion failed: \(error)")
                message = .error(.key("settings.account.delete.error.generic"))
            }
        } catch let error as WebauthnClientError {
            if FreshAuthRefusal.isRequired(error) {
                // The proof ran out on the way: the block appears, with no
                // error styling. Nothing was deleted.
                authRepository.dropFreshAuth()
                proof.proofExpired()
            } else if let untilMs = RecallWindowOpen.untilMs(of: error) {
                // ADR-0074 section 1.3 — the account's keys were reset (or
                // its password changed with the phrase) recently: it cannot
                // be deleted while it can still be taken back. Nothing was
                // deleted; the dialog says until when.
                message = .error(Self.deleteBlocked(untilMs: untilMs))
            } else if case .network = error {
                message = .error(.key("common.error.offline"))
            } else {
                NSLog("[account] WARN deletion failed: \(error)")
                message = .error(.key("settings.account.delete.error.generic"))
            }
        } catch {
            NSLog("[account] WARN deletion failed: \(error)")
            message = .error(.key("settings.account.delete.error.generic"))
        }
        return false
    }

    /// The server-side cleanups after `DELETE /account` (best-effort, run by
    /// ``AccountDeletionRun``): the directory rows, the backup blob, and this
    /// device's push-gw subscriptions, which no server can name.
    static func deletionCleanups(
        sessionToken: String,
        subscriptionIds: [Data],
        pushTokens: ReservedAnonTokens,
        announceDeletion: (@MainActor () async throws -> Void)? = nil
    ) -> [AccountDeletionCleanup] {
        var cleanups = [
            AccountDeletionCleanup(name: "discovery") {
                try await DiscoveryClient(sessionToken: sessionToken).deleteAccount()
            },
            AccountDeletionCleanup(name: "backup") {
                let storage = StorageClient()
                storage.setSessionToken(sessionToken)
                try await storage.deleteBackup()
            },
        ]
        // ADR-0098 section 1 — the account-deleted statement in every
        // conversation. A cleanup, so it runs only once the auth delete
        // succeeded and always before the wipe; the DS needs no session
        // (each post is proved by the group's epoch key).
        if let announceDeletion {
            cleanups.append(AccountDeletionCleanup(name: "conversations", run: announceDeletion))
        }
        if !subscriptionIds.isEmpty {
            // One client for every attempt, so an id removed by an earlier
            // attempt is not sent again.
            let push = PushClient(tokens: pushTokens)
            var remaining = subscriptionIds
            cleanups.append(AccountDeletionCleanup(name: "push") {
                var failed: [Data] = []
                var lastError: Error?
                for subId in remaining {
                    do {
                        try await push.unsubscribe(subId: subId)
                    } catch {
                        failed.append(subId)
                        lastError = error
                    }
                }
                remaining = failed
                if let lastError { throw lastError }
            })
        }
        return cleanups
    }

    /// `settings.account.delete.blocked` with the date the server named.
    static func deleteBlocked(untilMs: Int64) -> UiText {
        UiText {
            Localization.text(
                RecallCopy.deleteBlocked,
                texts: ["date": RecallCopy.date(ms: untilMs, locale: Localization.currentLocale)])
        }
    }

    /// A proof block's model for one of this screen's surfaces.
    func makeProof(_ surface: FreshAuthSurface) -> FreshAuthProofModel {
        FreshAuthProofModel(surface: surface, repository: authRepository)
    }

    /// The dialog closed: drop a line that belonged to it.
    public func clearMessage() {
        message = nil
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
