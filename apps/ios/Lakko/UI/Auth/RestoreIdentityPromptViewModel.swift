// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

/// State + dispatcher for ``RestoreIdentityPromptScreen``: "Restore your
/// keys".
///
/// Mirror of `apps/web/src/auth/RestoreIdentityPrompt.tsx`. Rendered only
/// while the auth state is ``AuthState/needsRecoveryPhrase(_:)``: the person
/// is signed in (a password, a passkey, an authenticator code) on a device
/// that does not hold the account's keys. The identity is never derived from
/// the password and never minted silently (either would break every
/// contact's pin); the keys come from one of three routes, in this order:
///
///   1. another device that is still signed in;
///   2. a passkey that carries the keys (the row is hidden when the account
///      has none);
///   3. the 12-word recovery phrase, checked fail-closed against the
///      account's published identity.
///
/// Never three words or the authenticator: those only sign in, and the
/// person is already signed in. A person with none of the three can start
/// over with a new identity.
@MainActor
public final class RestoreIdentityPromptViewModel: ObservableObject {

    /// The routes of the prompt.
    public enum Method: String, Hashable, CaseIterable {
        /// ADR-0075 — a code sent to the recovery email, while the server
        /// holds the email vault: the same flow as in **Get back in**.
        case email
        case device
        case passkey
        case phrase
    }

    @Published public var phrase: String = ""
    @Published public private(set) var busy: Bool = false

    /// UX 15.1 — the typed confirmation of "Reset account keys". The button
    /// is enabled only while it matches ``KeyResetConfirmation/word``.
    @Published public var resetConfirmation: String = ""
    /// UX 17.2 — the shared proof block of the warning screen: the password
    /// (and the username when the device knows none), or a passkey. Absent
    /// while a proof is live. Never pre-filled; cleared when the panel
    /// closes.
    let resetProof: FreshAuthProofModel
    /// UX 17.2 — `I know my password` on the "cannot reset" panel.
    @Published public private(set) var resetChoseKnowsPassword: Bool = false
    /// UX 17.2 — `I do not know my password` under the block.
    @Published public private(set) var resetChoseNoPassword: Bool = false
    /// `true` from the tap on "Reset account keys" until the server step
    /// answered: the full-screen "Resetting..." with no cancel. Covers the
    /// password sign-in and the reset as one state.
    @Published public private(set) var resetting: Bool = false
    /// A retry wait the reset's `429` asked for, in seconds; `nil` otherwise.
    @Published public private(set) var resetRetryAfterSecs: Int?
    /// Kept as ``UiText`` so a message on screen follows a live language switch.
    @Published private var errorText: UiText?
    public var error: String? { errorText?.text }

    /// How many of the account's passkeys carry keys; `nil` while it is not
    /// known, which keeps the passkey row.
    @Published public private(set) var passkeysWithKeys: Int?

    /// The username typed for the another-device route when the signed-in
    /// account has none on this device (a passkey sign-in names no account).
    @Published public var typedUsername: String = ""
    /// The typed username once confirmed with Continue: the pairing request
    /// is made for this name, not for every keystroke.
    @Published public private(set) var confirmedTypedUsername: String?

    /// Username for the account being restored; empty when the sign-in did
    /// not name one.
    public let username: String

    /// Base64 account id for the restore-via-device panel.
    public let accountIdB64: String

    /// G-56 (UX identity-recovery section 8a) — the variant for a device that
    /// holds the keys but not the account's EARLIER keys
    /// (``AuthState/needsEarlierKeys(_:)``): only "Another device" and the
    /// reset; the passkey, phrase and email rows bring keys this device has.
    public let earlierKeys: Bool

    /// Raw 16-byte account id decoded from ``accountIdB64``; `nil`
    /// only on a malformed stored marker.
    public var accountIdBytes: Data? {
        base64Decode(accountIdB64)
    }

    private let repository: AuthRepository
    private let l10n = Localization.shared

    public init(
        repository: AuthRepository,
        username: String,
        accountIdB64: String,
        earlierKeys: Bool = false,
        noteOwnAction: @escaping @MainActor (AccountNoticeKind) -> Void = { _ in },
        keepAddress: @escaping @MainActor (String) -> Void = { _ in }
    ) {
        self.repository = repository
        self.username = username
        self.accountIdB64 = accountIdB64
        self.earlierKeys = earlierKeys
        self.resetProof = FreshAuthProofModel(surface: .reset, repository: repository)
        self.email = EmailRecoverModel(
            ports: .live(repository: repository, noteOwnAction: noteOwnAction, keepAddress: keepAddress),
            cachedUsername: username.isEmpty ? nil : username)
        // The block's fields decide whether the button is enabled.
        proofSink = resetProof.objectWillChange.sink { [weak self] _ in
            self?.objectWillChange.send()
        }
    }

    private var proofSink: AnyCancellable?

    /// The rows for this device and account, in display order. Passkeys are
    /// supported on every iOS version this app runs on, so the passkey row
    /// depends only on whether the account has a passkey that carries keys.
    public var methods: [Method] {
        if earlierKeys { return [.device] }
        let rows = RestoreKeysRows.decide(protection)
        return Method.allCases.filter { method in
            switch method {
            case .email: return rows.showsEmailCode
            case .device: return true
            case .passkey:
                return PasskeyVault.restoreOffersPasskey(
                    passkeysSupported: true, passkeysWithKeys: passkeysWithKeys)
            case .phrase: return rows.showsPhrase
            }
        }
    }

    /// The account's protection (`GET /recovery-email`); `nil` until read.
    @Published public private(set) var protection: RecoveryEmailStatus?

    /// The email-code route, with the username pre-filled.
    let email: EmailRecoverModel

    /// G-56 contract 4.2 — while the parked screen is in the foreground the
    /// device looks for the earlier keys every 5 minutes (chosen, not
    /// measured). No progress, no counter.
    public static let earlierKeysRetryInterval: UInt64 = 5 * 60 * 1_000_000_000

    /// One look for the earlier keys (the backup, the account's own link).
    public func retryEarlierKeys() async {
        guard earlierKeys else { return }
        await repository.retryEarlierKeys()
    }

    /// Read the account's protection with the parked session.
    public func loadProtection() async {
        guard !earlierKeys else { return }
        if let status = await repository.protectionForPendingRestore() { protection = status }
    }

    /// Ask which of the account's passkeys carry keys (with the parked
    /// session token).
    public func loadPasskeysWithKeys() async {
        guard !earlierKeys else { return }
        passkeysWithKeys = await repository.passkeysWithKeysForPendingRestore()
    }

    /// The login handle the another-device route names the account by: the
    /// account's own username, else the one typed here. `nil` until a name
    /// that derives a handle is available.
    public var pairingLoginHandle: Data? {
        let name = username.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            ? (confirmedTypedUsername ?? "") : username
        return ownLoginHandle(username: name)
    }

    /// Continue on the typed-username field of the another-device route.
    public func confirmTypedUsername() {
        guard ownLoginHandle(username: typedUsername) != nil else {
            errorText = UiText {
                checkUsername(self.typedUsername).inlineMessage
                    ?? Localization.text("recovery.username_required")
            }
            return
        }
        errorText = nil
        confirmedTypedUsername = typedUsername
    }

    /// Finish the pending restore with what ANOTHER DEVICE handed over (the
    /// phrase entropy, recovery payload v2) instead of a typed phrase. Async
    /// passthrough to
    /// ``AuthRepository/completeFromPairing(phraseEntropy:session:)``;
    /// verification failures (`PhraseMismatch` /
    /// `IdentityVerificationUnavailable`) propagate to the calling panel,
    /// which surfaces them inline. On success the repository flips the auth
    /// state to `.session`. The entropy is zeroed in the outcome either way.
    public func completeFromPairing(
        outcome: PairingOrchestrator.JoinRecoveryOutcome
    ) async throws {
        defer { outcome.wipeEntropy() }
        try await repository.completeFromPairing(
            phraseEntropy: outcome.phraseEntropy,
            session: outcome.session,
            predecessors: outcome.predecessors
        )
    }

    /// Submit the recovery phrase. On success the auth state flips to
    /// ``AuthState/session(_:)`` and the surrounding router re-renders
    /// the protected content — no navigation needed here. On a typed
    /// mismatch / verification-unavailable, the error surfaces inline.
    public func onSubmit() {
        guard !busy else { return } // ignore double-tap
        if phrase.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            errorText = .key("recovery_phrase.error.incomplete")
            return
        }
        busy = true
        errorText = nil
        let phraseSnapshot = phrase
        Task {
            do {
                // Typed by the person: they hold it (ADR-0077 decision 3).
                try await repository.provideRecoveryPhrase(
                    mnemonicPhrase: phraseSnapshot,
                    phraseHeldByPerson: true
                )
                busy = false
                errorText = nil
                phrase = ""
            } catch {
                busy = false
                // Overtaken by a sign-out or another sign-in: not a failure.
                if error is StaleBindingError { return }
                self.errorText = UiText { AuthErrorCopy.identityRestore(error) }
            }
        }
    }

    /// The passkey row: run the ceremony for the signed-in account and ask
    /// the passkey for the keys. `true` when the row's form should close
    /// (the passkey carried no usable keys: the list comes back with the
    /// notice on top, and the count is re-read so a passkey that carried
    /// none is no longer offered).
    public func restoreWithPasskey() async -> Bool {
        guard !busy else { return false }
        busy = true
        errorText = nil
        defer { busy = false }
        do {
            let outcome = try await repository.restoreKeysWithPasskey()
            if outcome == .restored { return false }
            await loadPasskeysWithKeys()
            return true
        } catch {
            // Dismissing the sheet is not an error: back to idle.
            if !AuthErrorCopy.isPasskeyCancelled(error) {
                self.errorText = UiText { AuthErrorCopy.passkey(error) }
            }
            return false
        }
    }

    /// Whether the typed confirmation enables "Reset account keys".
    public var resetConfirmed: Bool {
        KeyResetConfirmation.matches(resetConfirmation)
    }

    /// UX 17.2 — which form the warning screen takes under its bullets.
    var resetForm: KeyResetProofGate.Form {
        KeyResetProofGate.form(
            holdsLiveProof: resetProof.proofLive,
            landing: repository.sessionLanding,
            hasUsablePasskey: resetProof.hasUsablePasskey,
            choseKnowsPassword: resetChoseKnowsPassword,
            choseNoPassword: resetChoseNoPassword)
    }

    /// Whether `I do not know my password` is under the block.
    public var showsResetNoPasswordLink: Bool {
        KeyResetProofGate.showsNoPasswordLink(
            form: resetForm, hasUsablePasskey: resetProof.hasUsablePasskey)
    }

    /// Whether "Reset account keys" is enabled: the word, and the block's
    /// fields when it is drawn.
    public var canReset: Bool {
        KeyResetProofGate.canSubmit(
            form: resetForm, blockReady: resetProof.layout.ready, confirmation: resetConfirmation)
    }

    /// `I know my password` / `I do not know my password`.
    public func resetKnowsPassword(_ knows: Bool) {
        resetChoseKnowsPassword = knows
        resetChoseNoPassword = !knows
    }

    /// The warning panel opened: read whether a proof is live and whether a
    /// passkey can stand in for the password.
    public func resetPanelOpened() {
        Task { await resetProof.opened() }
    }

    /// The warning panel closed (Cancel, collapse, the screen went away):
    /// the password is not kept.
    public func resetPanelClosed() {
        resetProof.closed()
        resetConfirmation = ""
        resetChoseKnowsPassword = false
        resetChoseNoPassword = false
    }

    /// The person has NO other device, NO passkey that restores keys and NO
    /// phrase, and typed the confirmation word. Reset the account keys via
    /// ``AuthRepository/mintFreshIdentity()``; the repository flips the auth
    /// state to `.saveFreshPhrase` and the router shows the new phrase (or
    /// its "could not finish publishing" form). A failure BEFORE anything
    /// changed leaves an inline message under the button (UX 15.1): the
    /// `429` line with its wait, the offline line, or the generic one.
    public func startFresh() {
        guard !busy, canReset else { return }
        busy = true
        resetting = true
        errorText = nil
        resetRetryAfterSecs = nil
        Task {
            // The proof first (the block's own states say why not); the
            // person sees one "Resetting..." for both.
            guard await resetProof.ensure() else {
                busy = false
                resetting = false
                return
            }
            do {
                _ = try await repository.mintFreshIdentity()
                // Repository flipped to .saveFreshPhrase — clear busy.
                busy = false
                resetting = false
                errorText = nil
            } catch {
                busy = false
                resetting = false
                // Overtaken by a sign-out or another sign-in: not a failure.
                if error is StaleBindingError { return }
                if FreshAuthRefusal.isRequired(error) {
                    // The proof expired on the way: show the block with its
                    // helper. Nothing went wrong, so no error styling.
                    resetProof.proofExpired()
                    return
                }
                NSLog("[restore] WARN key reset failed: \(error)")
                if error is FreshPhraseKeep.NotStored {
                    // Refused before anything was sent (ADR-0077 decision 3).
                    resetRetryAfterSecs = nil
                    self.errorText = .key("restore_identity.lost.error.phrase_not_stored")
                } else if let secs = RetryCountdown.attemptsRetryAfterSecs(error) {
                    resetRetryAfterSecs = secs
                    errorText = nil
                } else if case .network = error as? AuthClientError {
                    self.errorText = .key("restore_identity.reset.error.offline")
                } else {
                    self.errorText = .key("restore_identity.reset.error.generic")
                }
            }
        }
    }

    /// Sign out of the pending session. Returns to the anon surface.
    public func onSignOut() {
        repository.logout()
    }

    /// Drop a stale inline error (the screen switches method).
    public func clearError() {
        errorText = nil
    }
}

/// UX 15.1 — the word typed to confirm a key reset. The same Latin letters
/// in every language (like the slot numbers of the grid); compared after
/// trimming, case-sensitive.
/// The return key of the fields on the reset warning (UX 15.1, 17.2): it
/// does what "Reset account keys" does, under the button's own guards; from
/// the password field, while the word is not typed yet, it moves on to the
/// word.
enum KeyResetReturnKey {
    enum Field: Equatable { case password, word }
    enum Action: Equatable {
        /// Run the reset, as the button would.
        case submit
        /// Put the cursor in the confirmation-word field.
        case focusWord
        case nothing
    }

    /// Whether "Reset account keys" is enabled.
    static func buttonEnabled(busy: Bool, canReset: Bool, waiting: Bool) -> Bool {
        !busy && canReset && !waiting
    }

    static func action(from field: Field, buttonEnabled: Bool, wordTyped: Bool) -> Action {
        if buttonEnabled { return .submit }
        if field == .password, !wordTyped { return .focusWord }
        return .nothing
    }
}

enum KeyResetConfirmation {
    static let word = "RESET"

    static func matches(_ typed: String) -> Bool {
        typed.trimmingCharacters(in: .whitespacesAndNewlines) == word
    }
}
