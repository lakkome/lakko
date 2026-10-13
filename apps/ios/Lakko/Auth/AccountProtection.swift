// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0075 / docs/ux/identity-recovery.md section 19 — what protects an
// account: a recovery email, a saved recovery phrase, or nothing.
//
// This file is the pure part and the per-device record:
//
//   * `RecoveryEmailStatus`     what `GET /recovery-email` answers;
//   * `AccountProtection`       the account's mode;
//   * `ProtectionRows`          what Settings > Account draws from it;
//   * `ProtectBanner`           when the no-protection banner is on screen;
//   * `EnrolSealing`            whether an enrolment seals a vault;
//   * `AccountProtectionStore`  the per-account flags kept on the device.
//
// The address itself is never part of any of it: it is typed, used and
// dropped, and nothing here could store it.

/// `GET /recovery-email`.
public struct RecoveryEmailStatus: Codable, Equatable, Sendable {
    /// Whether this server can send a recovery code.
    public let available: Bool
    /// The account has a recovery email. Never the address.
    public let enabled: Bool
    /// The account has an email vault: a mailed code brings its keys back.
    public let vault: Bool
    /// The owner saved a recovery phrase.
    public let phraseSaved: Bool
    /// The account has an authenticator: its code is asked for as well as
    /// the mailed one. `nil` from a server that does not say.
    public let totp: Bool?

    public init(available: Bool, enabled: Bool, vault: Bool, phraseSaved: Bool, totp: Bool? = nil) {
        self.available = available
        self.enabled = enabled
        self.vault = vault
        self.phraseSaved = phraseSaved
        self.totp = totp
    }

    enum CodingKeys: String, CodingKey {
        case available, enabled, vault, totp
        case phraseSaved = "phrase_saved"
    }
}

/// The account's mode (UX 19.1).
public enum AccountProtection: Equatable, Sendable {
    /// No phrase saved, and a mailed code brings everything back.
    case email
    /// A recovery phrase was saved.
    case phrase
    /// Neither: the account rests on the password and its devices.
    case none
}

/// The one rule. A saved phrase wins. Without one the account is protected
/// by its email only while the server holds the vault: an email without a
/// vault (the keys were reset or the account was taken back, and the server
/// dropped the vault) changes the password and brings nothing back, so the
/// account reads as unprotected until the email is added again.
public func accountProtection(enabled: Bool, vault: Bool, phraseSaved: Bool) -> AccountProtection {
    if phraseSaved { return .phrase }
    return enabled && vault ? .email : .none
}

public extension RecoveryEmailStatus {
    var protection: AccountProtection {
        accountProtection(enabled: enabled, vault: vault, phraseSaved: phraseSaved)
    }

    /// The email is on but the server holds no vault for an account without
    /// a saved phrase (a key reset, a rotation or a recall dropped it).
    var vaultMissing: Bool { enabled && !vault && !phraseSaved }

    /// `vaultMissing` with no device of the account holding the address:
    /// the person adds the email again. (A device that holds the address
    /// re-seals by itself: ``EmailReseal``.)
    var emailNeedsAddingAgain: Bool { needsAddingAgain(addressHeld: false) }

    func needsAddingAgain(addressHeld: Bool) -> Bool { vaultMissing && !addressHeld }
}

/// ADR-0075, amended 2026-10-13 — the automatic re-seal: a device that
/// holds the address puts the email vault back by itself, silently.
public enum EmailReseal {

    /// The one rule, evaluated after every read of `GET /recovery-email`.
    public static func resealNeeded(
        enabled: Bool, vault: Bool, phraseSaved: Bool, addressHeld: Bool
    ) -> Bool {
        enabled && !vault && !phraseSaved && addressHeld
    }

    /// What one `PUT /recovery-email/vault` came to.
    public enum Outcome: Equatable {
        /// Stored, or as good as: a vault exists already, or a phrase was
        /// saved (no vault is wanted). The status is read again.
        case done
        /// `403 address-mismatch`: the address this device holds is not the
        /// account's any more. The register is cleared; "add your email
        /// again" takes over.
        case addressStale
        /// Anything else: tried again at the next landing, never in a loop
        /// (every well-formed request spends one of ten per day).
        case retryAtNextLanding
    }

    public static func outcome(status: Int, body: String) -> Outcome {
        let code = ((try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any])?["error"]
            as? String
        switch (status, code) {
        case (200..<300, _): return .done
        case (409, "vault-exists"), (409, "phrase-saved"): return .done
        case (403, "address-mismatch"): return .addressStale
        default: return .retryAtNextLanding
        }
    }

    public static func outcome(of error: Error?) -> Outcome {
        guard let error else { return .done }
        if case .http(let status, _, let body) = error as? AuthClientError {
            return outcome(status: status, body: body)
        }
        return .retryAtNextLanding
    }

    /// The address the enrol form starts with: nothing when an address is
    /// being replaced (a new one is typed), else the one this device holds
    /// (after a recall the stored key is gone and the same address is
    /// confirmed by a letter again).
    public static func prefill(replacing: Bool, addressHeld: String?) -> String {
        replacing ? "" : (addressHeld ?? "")
    }
}

/// What Settings > Account draws (UX 19.1, 19.2, 19.5), pure.
public struct ProtectionRows: Equatable {

    public enum EmailRow: Equatable {
        /// Mail is not set up on this server and the account has no email:
        /// no row at all.
        case absent
        /// `Recovery email: Off`, opening the enrolment.
        case off
        /// `Recovery email: On` with `Replace` and `Remove`.
        case on
        /// The email is on but the server dropped its vault (a key reset, a
        /// recall or a rotation on an account without a saved phrase):
        /// `recovery_email.add_again.*`, opening the enrolment.
        case addAgain
    }

    public enum PhraseRow: Equatable {
        /// `Save a recovery phrase`.
        case save
        /// `Show recovery phrase`: only a phrase-protected account has it.
        case show
    }

    /// `protect.status.*`.
    public let statusKey: String
    public let email: EmailRow
    public let phrase: PhraseRow
    /// Whether any "three words" surface exists for this account.
    public let showsThreeWords: Bool

    public static func decide(_ status: RecoveryEmailStatus, addressHeld: Bool = false) -> ProtectionRows {
        let statusKey: String
        switch status.protection {
        case .email: statusKey = "protect.status.email"
        case .phrase: statusKey = "protect.status.phrase"
        case .none: statusKey = "protect.status.none"
        }
        let email: EmailRow
        if status.needsAddingAgain(addressHeld: addressHeld) {
            email = .addAgain
        } else if status.enabled {
            email = .on
        } else {
            email = status.available ? .off : .absent
        }
        return ProtectionRows(
            statusKey: statusKey,
            email: email,
            phrase: status.phraseSaved ? .show : .save,
            showsThreeWords: status.phraseSaved)
    }

    /// Whether the sign-up step (19.1a) offers `Add an email`.
    public static func signupOffersEmail(available: Bool, hasUsername: Bool) -> Bool {
        available && hasUsername
    }
}

/// The "Protect your account" step (UX 19.1a), pure: which rows it has,
/// which are already done, and what stands under them.
public struct ProtectStep: Equatable {

    public enum Row: Hashable, Sendable {
        case email
        case passkey
        case phrase
    }

    public struct Item: Equatable {
        public let row: Row
        /// Marked `protect.done`.
        public let done: Bool
    }

    public enum Footer: Equatable {
        /// The plain `Not now` (with the app's own confirmation).
        case notNow
        /// The primary `protect.continue`: something was done.
        case continueOn
        /// Nothing (the Settings entry, which the back button leaves).
        case none
    }

    /// Email, passkey, phrase, in that order.
    public let items: [Item]
    public let footer: Footer

    /// - Parameters:
    ///   - offersEmail: mail is set up and the account has a username.
    ///   - passkeysSupported: this device can use passkeys.
    ///   - done: the rows already done.
    ///   - leavable: the step has its own way out (after sign-up).
    public static func decide(
        offersEmail: Bool, passkeysSupported: Bool, done: Set<Row>, leavable: Bool
    ) -> ProtectStep {
        var rows: [Row] = []
        if offersEmail || done.contains(.email) { rows.append(.email) }
        if passkeysSupported { rows.append(.passkey) }
        rows.append(.phrase)
        let items = rows.map { Item(row: $0, done: done.contains($0)) }
        let footer: Footer
        if !leavable {
            footer = .none
        } else {
            footer = items.contains(where: \.done) ? .continueOn : .notNow
        }
        return ProtectStep(items: items, footer: footer)
    }

    /// The rows the account's status marks done: the email while it
    /// protects (on, and not waiting to be added again), the phrase once
    /// saved, the passkey when the account has one.
    public static func done(status: RecoveryEmailStatus?, hasPasskey: Bool) -> Set<Row> {
        var done: Set<Row> = []
        if let status, status.enabled, !status.vaultMissing { done.insert(.email) }
        if status?.phraseSaved == true { done.insert(.phrase) }
        if hasPasskey { done.insert(.passkey) }
        return done
    }
}

/// What "Restore your keys" (UX section 8: signed in, no keys on this
/// device) offers besides another device and a passkey, pure (ADR-0075).
public struct RestoreKeysRows: Equatable {
    /// The 12-word phrase row: hidden for an account whose phrase was never
    /// saved (nobody has words to type). Kept while the status is unknown.
    public let showsPhrase: Bool
    /// The `Email code` row: only while the server holds the email vault,
    /// since only then does a mailed code bring the keys.
    public let showsEmailCode: Bool

    public static func decide(_ status: RecoveryEmailStatus?) -> RestoreKeysRows {
        guard let status else { return RestoreKeysRows(showsPhrase: true, showsEmailCode: false) }
        return RestoreKeysRows(
            showsPhrase: status.phraseSaved,
            showsEmailCode: status.enabled && status.vault)
    }
}

/// The dismissible "This account has no way back in." banner (UX 19.1a):
/// only for an unprotected account, and back 30 days after a dismissal if
/// nothing changed.
public enum ProtectBanner {

    public static let returnsAfterMs: Int64 = 30 * 24 * 60 * 60 * 1000

    /// What the banner says: the add-again copy when the email is on and its
    /// vault is gone, otherwise the ordinary "no way back in" line.
    public struct Copy: Equatable {
        public let titleKey: String
        public let bodyKey: String?
        public let actionKey: String
    }

    public static func copy(_ status: RecoveryEmailStatus, addressHeld: Bool = false) -> Copy {
        status.needsAddingAgain(addressHeld: addressHeld)
            ? Copy(
                titleKey: "recovery_email.add_again.title", bodyKey: "recovery_email.add_again.body",
                actionKey: "protect.email.title")
            : Copy(titleKey: "protect.banner.none", bodyKey: nil, actionKey: "protect.banner.action")
    }

    /// - Parameter hasPasskey: the account has at least one passkey: it has
    ///   a way back in, so the banner (and its 30-day return) is not shown.
    /// - Parameter resealing: the vault is missing and this device holds the
    ///   address, so it is putting the vault back by itself: nothing to ask.
    public static func shows(
        protection: AccountProtection, hasPasskey: Bool = false, resealing: Bool = false,
        dismissedAtMs: Int64?, nowMs: Int64
    ) -> Bool {
        guard protection == .none, !hasPasskey, !resealing else { return false }
        guard let dismissedAtMs else { return true }
        // A clock that went backwards reads as "not yet".
        return nowMs >= dismissedAtMs && nowMs - dismissedAtMs >= returnsAfterMs
    }
}

/// Whether an enrolment seals the email vault (ADR-0075 section 6): only
/// while no phrase is saved. With a saved phrase the mailed code changes the
/// password and nothing else, and the server never gets a vault.
public enum EnrolSealing {
    public static func sealsVault(phraseSaved: Bool) -> Bool { !phraseSaved }
}

/// What this device keeps about the protection of one account, in
/// `UserDefaults`. Flags only: never the address, never a code. Cleared by
/// the account wipe (``AccountLocalDataWipe``).
public struct AccountProtectionStore {
    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    static let prefix = "lakko.account-protection.v1."

    static func key(_ name: String, accountIdB64: String) -> String {
        "\(prefix)\(name).\(accountIdB64)"
    }

    /// Every key of `accountIdB64`, for the wipe.
    static func keys(accountIdB64: String) -> [String] {
        ["phrase-saved", "status", "banner-dismissed", "notice-cursor", "own-actions", "first-password-owed",
         "rename-owed", "rename-at", "username-register-owed"]
            .map { key($0, accountIdB64: accountIdB64) }
    }

    // MARK: The phrase-saved flag

    /// Whether the account's recovery phrase was saved (on this device, or
    /// on another one and learned at a landing). The three-word surfaces and
    /// `Show recovery phrase` exist only then.
    public func phraseSaved(accountIdB64: String) -> Bool {
        defaults.bool(forKey: Self.key("phrase-saved", accountIdB64: accountIdB64))
    }

    public func setPhraseSaved(_ saved: Bool, accountIdB64: String) {
        defaults.set(saved, forKey: Self.key("phrase-saved", accountIdB64: accountIdB64))
    }

    // MARK: A claimed username whose first password is still owed

    /// A guest chose a username on this device and the account's first
    /// password has not been set yet (the password it has is random and
    /// known to nobody). Until it is, the password form opens without a
    /// proof: there is nothing that could be proven.
    public func firstPasswordOwed(accountIdB64: String) -> Bool {
        defaults.bool(forKey: Self.key("first-password-owed", accountIdB64: accountIdB64))
    }

    public func setFirstPasswordOwed(_ owed: Bool, accountIdB64: String) {
        if owed {
            defaults.set(true, forKey: Self.key("first-password-owed", accountIdB64: accountIdB64))
        } else {
            defaults.removeObject(forKey: Self.key("first-password-owed", accountIdB64: accountIdB64))
        }
    }

    // MARK: Changing the username (ADR-0076)

    /// The rename this device started and has not finished.
    public func renameOwed(accountIdB64: String) -> UsernameRename.Record? {
        defaults.data(forKey: Self.key("rename-owed", accountIdB64: accountIdB64))
            .flatMap { try? JSONDecoder().decode(UsernameRename.Record.self, from: $0) }
    }

    public func setRenameOwed(_ record: UsernameRename.Record?, accountIdB64: String) {
        let key = Self.key("rename-owed", accountIdB64: accountIdB64)
        if let record, let data = try? JSONEncoder().encode(record) {
            defaults.set(data, forKey: key)
        } else {
            defaults.removeObject(forKey: key)
        }
    }

    /// When this device last changed the username (unix ms).
    public func lastRenameAtMs(accountIdB64: String) -> Int64? {
        defaults.object(forKey: Self.key("rename-at", accountIdB64: accountIdB64)) as? Int64
    }

    public func setLastRenameAtMs(_ ms: Int64, accountIdB64: String) {
        defaults.set(ms, forKey: Self.key("rename-at", accountIdB64: accountIdB64))
    }

    /// A name this device wrote locally and still owes the own-username
    /// register (the synced state was not loaded, or the write failed).
    public func usernameRegisterOwed(accountIdB64: String) -> String? {
        defaults.string(forKey: Self.key("username-register-owed", accountIdB64: accountIdB64))
    }

    public func setUsernameRegisterOwed(_ name: String?, accountIdB64: String) {
        let key = Self.key("username-register-owed", accountIdB64: accountIdB64)
        if let name { defaults.set(name, forKey: key) } else { defaults.removeObject(forKey: key) }
    }

    // MARK: The last status read

    public func status(accountIdB64: String) -> RecoveryEmailStatus? {
        guard let data = defaults.data(forKey: Self.key("status", accountIdB64: accountIdB64)) else {
            return nil
        }
        return try? JSONDecoder().decode(RecoveryEmailStatus.self, from: data)
    }

    public func setStatus(_ status: RecoveryEmailStatus, accountIdB64: String) {
        guard let data = try? JSONEncoder().encode(status) else { return }
        defaults.set(data, forKey: Self.key("status", accountIdB64: accountIdB64))
        setPhraseSaved(status.phraseSaved, accountIdB64: accountIdB64)
    }

    // MARK: The banner

    public func bannerDismissedAtMs(accountIdB64: String) -> Int64? {
        defaults.object(forKey: Self.key("banner-dismissed", accountIdB64: accountIdB64)) as? Int64
    }

    public func setBannerDismissed(atMs: Int64, accountIdB64: String) {
        defaults.set(atMs, forKey: Self.key("banner-dismissed", accountIdB64: accountIdB64))
    }

    // MARK: Notices

    /// The id of the last notice this device has seen; `nil` on a device
    /// that never kept a cursor (it then asks for the last 30 days).
    public func noticeCursor(accountIdB64: String) -> UInt64? {
        (defaults.object(forKey: Self.key("notice-cursor", accountIdB64: accountIdB64)) as? Int64)
            .map { UInt64(max(0, $0)) }
    }

    public func setNoticeCursor(_ cursor: UInt64, accountIdB64: String) {
        defaults.set(
            Int64(clamping: cursor), forKey: Self.key("notice-cursor", accountIdB64: accountIdB64))
    }

    /// "I did this" markers: notice kind -> when this device did it (unix
    /// ms).
    public func ownActions(accountIdB64: String) -> [String: Int64] {
        let stored = defaults.dictionary(forKey: Self.key("own-actions", accountIdB64: accountIdB64))
        return (stored as? [String: Int64]) ?? [:]
    }

    public func markOwnAction(_ kind: AccountNoticeKind, atMs: Int64, accountIdB64: String) {
        var actions = ownActions(accountIdB64: accountIdB64)
        actions[kind.rawValue] = atMs
        defaults.set(actions, forKey: Self.key("own-actions", accountIdB64: accountIdB64))
    }

    /// A marker did its work: it suppresses one notice, not every later one.
    public func clearOwnAction(_ kind: String, accountIdB64: String) {
        var actions = ownActions(accountIdB64: accountIdB64)
        actions[kind] = nil
        defaults.set(actions, forKey: Self.key("own-actions", accountIdB64: accountIdB64))
    }

    public func clear(accountIdB64: String) {
        for key in Self.keys(accountIdB64: accountIdB64) {
            defaults.removeObject(forKey: key)
        }
    }
}

/// ADR-0075 — whether the new words of a key reset or a recall are shown:
/// only to a phrase-protected account. (A rotation asks
/// ``KeyRotationPlan/showsNewWords(savesPhrase:phraseProtected:)``.)
public enum NewWordsRule {
    public static func shown(phraseProtected: Bool) -> Bool { phraseProtected }
}

/// The text of the publish-owed / retry screen after a key reset or a
/// take-back (ADR-0075): an account that is not phrase-protected is told
/// its keys are still being published, with no mention of words.
public enum KeysChangedCopy {
    public static func publishOwedKey(showsWords: Bool, needsSignIn: Bool) -> String {
        if needsSignIn { return "recovery_phrase.fresh.sign_in_to_finish" }
        return showsWords ? "recovery_phrase.fresh.publish_owed" : "account.keys_changed.finishing"
    }
}
