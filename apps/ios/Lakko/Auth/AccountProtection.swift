// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0075 (amended 2026-11-26) / docs/ux/identity-recovery.md section 19 —
// what protects an account.
//
// Since 2026-11-26 there is one kind of account. Every account with a
// username has at least one SECOND FACTOR (a recovery email, an
// authenticator app, a passkey): a key reset asks for the password and one
// of them. The 12-word phrase is an optional extra and is not one of the
// three. The email vault is gone: a mailed code gives a new password and
// confirms a reset, never the keys.
//
// Separately, the account's published protection (ADR-0101 section 10,
// ``AccountSeals``) says what brings the chats back after a reset: the
// phrase once saved, a passkey that keeps the chats, or the password alone.
//
// This file is the pure part and the per-device record:
//
//   * `RecoveryEmailStatus`     what `GET /recovery-email` answers;
//   * `AccountFactors`          the three second factors of the account;
//   * `ProtectStep`             the "Protect your account" step (19.1a);
//   * `FactorLanding`           the landing rule (19.1c);
//   * `LastFactor`              the last one cannot be removed (19.1c);
//   * `AuthenticatorOnly`       the warning when it is the only one;
//   * `PasswordOnlyNotice`      "Your chats depend on your password" (19.1d);
//   * `ProtectionRows`          what Settings > Account draws;
//   * `AccountProtectionStore`  the per-account flags kept on the device.
//
// The address itself is never part of any of it: it is typed, used and
// dropped, and nothing here could store it.

/// `GET /recovery-email`: `{available, enabled, totp}`.
public struct RecoveryEmailStatus: Codable, Equatable, Sendable {
    /// Whether this server can send a recovery code.
    public let available: Bool
    /// The account has a recovery email. Never the address.
    public let enabled: Bool
    /// The account has a confirmed authenticator. `nil` from a server that
    /// does not say.
    public let totp: Bool?

    public init(available: Bool, enabled: Bool, totp: Bool? = nil) {
        self.available = available
        self.enabled = enabled
        self.totp = totp
    }
}

/// The account's second factors (UX 19.1a): what a key reset accepts after
/// the password.
public struct AccountFactors: Equatable, Sendable {
    public var email: Bool
    public var authenticator: Bool
    public var passkey: Bool

    public init(email: Bool, authenticator: Bool, passkey: Bool) {
        self.email = email
        self.authenticator = authenticator
        self.passkey = passkey
    }

    public var count: Int { [email, authenticator, passkey].filter { $0 }.count }
    public var any: Bool { count > 0 }
    /// The authenticator is the only one of the three.
    public var authenticatorOnly: Bool { authenticator && !email && !passkey }

    /// The factors as the server's status and the passkey list say them;
    /// `nil` while either is unknown (nothing is decided on a guess).
    public static func of(status: RecoveryEmailStatus?, hasPasskey: Bool?) -> AccountFactors? {
        guard let status, let totp = status.totp, let hasPasskey else { return nil }
        return AccountFactors(email: status.enabled, authenticator: totp, passkey: hasPasskey)
    }
}

/// The "Protect your account" step (UX 19.1a), pure: which rows it has,
/// which are done, whether `Continue` is enabled, and what stands under the
/// rows.
public struct ProtectStep: Equatable {

    public enum Row: Hashable, Sendable {
        case email
        case authenticator
        case passkey
        /// The optional extra: it does not count as one of the three.
        case phrase

        /// One of the three second factors.
        public var isFactor: Bool { self != .phrase }
    }

    public struct Item: Equatable {
        public let row: Row
        /// Marked `protect.done`.
        public let done: Bool
    }

    /// The note in the warning colour between the rows and `Continue`.
    public enum Warning: Equatable {
        /// `protect.totp_only.warning`.
        case authenticatorOnly
        /// `protect.totp_only.warning_phrase`: a phrase was saved as well.
        case authenticatorOnlyWithPhrase

        public var key: String {
            switch self {
            case .authenticatorOnly: return "protect.totp_only.warning"
            case .authenticatorOnlyWithPhrase: return "protect.totp_only.warning_phrase"
            }
        }
    }

    public enum Footer: Equatable {
        /// `protect.continue`, enabled or drawn disabled with
        /// `protect.continue_needs` under it. Never hidden, never `Not now`.
        case continueOn(enabled: Bool)
        /// Nothing (the Settings entry, which the back button leaves).
        case none
    }

    /// Email, authenticator, passkey: the three, in that order.
    public let factors: [Item]
    /// The optional phrase row, under the `protect.optional` caption.
    public let phrase: Item
    public let warning: Warning?
    /// The honest notice of 19.1d stands under the rows.
    public let showsPasswordOnlyNotice: Bool
    public let footer: Footer
    /// `protect.title`, or `protect.title_required` when the landing rule
    /// put the step in front of the app.
    public let titleKey: String

    /// - Parameters:
    ///   - offersEmail: mail is set up and the account has a username.
    ///   - passkeysSupported: this device can use passkeys and the account
    ///     has a username.
    ///   - done: the rows already done.
    ///   - keepsChats: a phrase is saved or a passkey keeps the chats
    ///     (``AccountSeals/keptBy`` is not the password alone).
    ///   - leavable: the step has its own `Continue` (after sign-up, and
    ///     at a landing); `false` for the Settings entry.
    ///   - required: shown by the landing rule (19.1c).
    public static func decide(
        offersEmail: Bool, passkeysSupported: Bool, done: Set<Row>, keepsChats: Bool,
        leavable: Bool, required: Bool = false
    ) -> ProtectStep {
        var rows: [Row] = []
        if offersEmail || done.contains(.email) { rows.append(.email) }
        // Always offered: the server refuses to start without it.
        rows.append(.authenticator)
        if passkeysSupported || done.contains(.passkey) { rows.append(.passkey) }
        let factors = rows.map { Item(row: $0, done: done.contains($0)) }
        let doneFactors = Set(factors.filter(\.done).map(\.row))
        let phraseDone = done.contains(.phrase)
        let warning: Warning?
        if doneFactors == [.authenticator] {
            warning = phraseDone ? .authenticatorOnlyWithPhrase : .authenticatorOnly
        } else {
            warning = nil
        }
        return ProtectStep(
            factors: factors,
            phrase: Item(row: .phrase, done: phraseDone),
            warning: warning,
            showsPasswordOnlyNotice: !doneFactors.isEmpty && !keepsChats && !phraseDone,
            footer: leavable ? .continueOn(enabled: !doneFactors.isEmpty) : .none,
            titleKey: required ? "protect.title_required" : "protect.title")
    }

    /// The rows the account's state marks done.
    public static func done(factors: AccountFactors?, phraseSaved: Bool) -> Set<Row> {
        var done: Set<Row> = []
        if factors?.email == true { done.insert(.email) }
        if factors?.authenticator == true { done.insert(.authenticator) }
        if factors?.passkey == true { done.insert(.passkey) }
        if phraseSaved { done.insert(.phrase) }
        return done
    }

    /// Whether the sign-up step offers `Add an email`.
    public static func offersEmail(available: Bool, hasUsername: Bool) -> Bool {
        available && hasUsername
    }
}

/// UX 19.1c — at every session landing an account with a username and none
/// of the three is shown the step before the app.
public enum FactorLanding {
    /// `nil` factors (not yet read, or the read failed) never put the step
    /// up: the question is asked again at the next landing. A guest account
    /// (no username) sees none of this.
    public static func needsStep(hasUsername: Bool, factors: AccountFactors?) -> Bool {
        guard hasUsername, let factors else { return false }
        return !factors.any
    }
}

/// UX 19.1c — the last of the three cannot be removed. The server refuses
/// it too (`409 last-factor`); the app's own dialog says
/// `protect.last_factor` first.
public enum LastFactor {

    public static let code = "last-factor"
    public static let messageKey = "protect.last_factor"

    /// Whether removing `row` would leave the account with none. Unknown
    /// factors refuse nothing here (the server still does).
    public static func refuses(removing row: ProtectStep.Row, factors: AccountFactors?) -> Bool {
        guard let factors, factors.count == 1 else { return false }
        switch row {
        case .email: return factors.email
        case .authenticator: return factors.authenticator
        case .passkey: return factors.passkey
        case .phrase: return false
        }
    }

    /// Removing the last PASSKEY of several is a removal of the factor;
    /// removing one of two passkeys is not.
    public static func refusesPasskeyRemoval(passkeyCount: Int, factors: AccountFactors?) -> Bool {
        passkeyCount <= 1 && refuses(removing: .passkey, factors: factors)
    }

    /// `409 {"error":"last-factor"}` from any service client.
    public static func isRefusal(_ error: Error) -> Bool {
        let status: Int
        let body: String
        if case .http(let s, _, let b) = error as? AuthClientError {
            (status, body) = (s, b)
        } else if case .http(let s, _, let b) = error as? WebauthnClientError {
            (status, body) = (s, b)
        } else {
            return false
        }
        guard status == 409 else { return false }
        let json = (try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any]
        return json?["error"] as? String == code
    }
}

/// UX 19.1a / 19.1c — the authenticator as the account's only second way
/// in.
public enum AuthenticatorOnly {

    /// The note at the top of Settings > Security > Recovery, for as long as
    /// the authenticator is the only one; it cannot be dismissed.
    public static func settingsWarning(factors: AccountFactors?, phraseSaved: Bool) -> ProtectStep.Warning? {
        guard factors?.authenticatorOnly == true else { return nil }
        return phraseSaved ? .authenticatorOnlyWithPhrase : .authenticatorOnly
    }

    /// The line a removal's confirmation adds before its buttons when the
    /// removal would leave only the authenticator; `nil` otherwise.
    public static func removalNoticeKey(
        removing row: ProtectStep.Row, passkeyCount: Int = 1, factors: AccountFactors?, phraseSaved: Bool
    ) -> String? {
        guard let factors, factors.authenticator else { return nil }
        var after = factors
        switch row {
        case .email: after.email = false
        case .passkey: if passkeyCount <= 1 { after.passkey = false }
        case .authenticator, .phrase: return nil
        }
        guard after != factors, after.authenticatorOnly else { return nil }
        return phraseSaved ? "protect.totp_only.remove_notice_phrase" : "protect.totp_only.remove_notice"
    }
}

/// UX 19.1d — "Your chats depend on your password": the account has a
/// second way in but no saved phrase and no passkey that keeps the chats.
public enum PasswordOnlyNotice {

    public static let titleKey = "protect.password_only.title"
    public static let bodyKey = "protect.password_only.body"
    public static let savePhraseKey = "protect.password_only.save_phrase"
    public static let addPasskeyKey = "protect.password_only.add_passkey"
    public static let passkeyNoSealKey = "protect.password_only.passkey_no_seal"

    /// Whether the account is kept by the password only. `nil` seals (the
    /// account's protection was never read on this device) show nothing.
    public static func applies(factors: AccountFactors?, seals: AccountSeals?) -> Bool {
        guard let factors, factors.any, let seals else { return false }
        return seals.keptBy == .passwordOnly
    }

    /// Place 3: once after a sign-in on a device, the first time the app
    /// opens there. Not after a key reset (its own screens said what was
    /// kept), and never again once closed on this device.
    public static func showsDialog(
        applies: Bool, closedOnThisDevice: Bool, afterKeyReset: Bool
    ) -> Bool {
        applies && !closedOnThisDevice && !afterKeyReset
    }

    /// The extra line: the account has a passkey and none keeps the chats.
    public static func showsPasskeyNoSeal(factors: AccountFactors?, seals: AccountSeals?) -> Bool {
        guard let factors, let seals else { return false }
        return factors.passkey && seals.passkeyTagsB64.isEmpty
    }
}

/// What Settings > Account draws (UX 19.1c, 19.2, 19.5), pure.
public struct ProtectionRows: Equatable {

    public enum EmailRow: Equatable {
        /// Mail is not set up on this server and the account has no email:
        /// no row at all.
        case absent
        /// `Recovery email: Off`, opening the enrolment.
        case off
        /// `Recovery email: On` with `Replace` and `Remove`.
        case on
    }

    public enum PhraseRow: Equatable {
        /// `Save a recovery phrase`.
        case save
        /// `Show recovery phrase`: only once it was saved.
        case show
    }

    /// `protect.status.phrase` / `.passkey_seal` / `.password_only`.
    public let statusKey: String
    /// The status line is drawn in the warning colour with the 19.1d
    /// actions.
    public let statusIsWarning: Bool
    public let email: EmailRow
    public let phrase: PhraseRow
    /// Whether any "three words" surface exists for this account.
    public let showsThreeWords: Bool

    public static func decide(_ status: RecoveryEmailStatus, seals: AccountSeals?) -> ProtectionRows {
        let kept = seals?.keptBy ?? .passwordOnly
        let statusKey: String
        switch kept {
        case .phrase: statusKey = "protect.status.phrase"
        case .passkey: statusKey = "protect.status.passkey_seal"
        case .passwordOnly: statusKey = "protect.status.password_only"
        }
        let email: EmailRow
        if status.enabled {
            email = .on
        } else {
            email = status.available ? .off : .absent
        }
        let phraseSaved = seals?.phraseSaved ?? false
        return ProtectionRows(
            statusKey: statusKey,
            // Unknown seals warn about nothing.
            statusIsWarning: seals != nil && kept == .passwordOnly,
            email: email,
            phrase: phraseSaved ? .show : .save,
            showsThreeWords: phraseSaved)
    }
}

/// What "Restore your keys" (UX section 8: signed in, no keys on this
/// device) offers besides another device and a passkey, pure. Since
/// 2026-11-26 a mailed code never brings the keys: there is no email row.
public struct RestoreKeysRows: Equatable {
    /// The 12-word phrase row. Always offered: whether the phrase was saved
    /// is told by the account's announcement, which a device without the
    /// keys may not have read, and hiding the row would strand a person who
    /// holds the words.
    public let showsPhrase: Bool

    public static func decide() -> RestoreKeysRows {
        RestoreKeysRows(showsPhrase: true)
    }
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
        // ("banner-dismissed" is no longer written; the wipe still clears
        // what an earlier build left.)
        ["phrase-saved", "status", "banner-dismissed", "notice-cursor", "own-actions",
         "rename-owed", "username-register-owed", "seals", "password-only-notice",
         "phrase-seal-owed"]
            .map { key($0, accountIdB64: accountIdB64) }
    }

    // MARK: The phrase-saved flag

    /// Whether the account's recovery phrase was saved: its published
    /// protection lists a phrase seal (ADR-0075, amended 2026-11-26: the
    /// server keeps no flag). Written from the announcement at every look
    /// at the account's own protection, and at once when the phrase is
    /// saved here. The three-word surfaces and `Show recovery phrase` exist
    /// only then.
    public func phraseSaved(accountIdB64: String) -> Bool {
        defaults.bool(forKey: Self.key("phrase-saved", accountIdB64: accountIdB64))
    }

    public func setPhraseSaved(_ saved: Bool, accountIdB64: String) {
        guard let gate = AccountStoreGate.enterUnlessWiped(accountIdB64) else { return }
        defer { gate.leave() }
        defaults.set(saved, forKey: Self.key("phrase-saved", accountIdB64: accountIdB64))
    }

    // MARK: Changing the username (ADR-0076)

    /// The rename this device started and has not finished.
    public func renameOwed(accountIdB64: String) -> UsernameRename.Record? {
        defaults.data(forKey: Self.key("rename-owed", accountIdB64: accountIdB64))
            .flatMap { try? JSONDecoder().decode(UsernameRename.Record.self, from: $0) }
    }

    public func setRenameOwed(_ record: UsernameRename.Record?, accountIdB64: String) {
        guard let gate = AccountStoreGate.enterUnlessWiped(accountIdB64) else { return }
        defer { gate.leave() }
        let key = Self.key("rename-owed", accountIdB64: accountIdB64)
        if let record, let data = try? JSONEncoder().encode(record) {
            defaults.set(data, forKey: key)
        } else {
            defaults.removeObject(forKey: key)
        }
    }

    /// A name this device wrote locally and still owes the own-username
    /// register (the synced state was not loaded, or the write failed).
    public func usernameRegisterOwed(accountIdB64: String) -> String? {
        defaults.string(forKey: Self.key("username-register-owed", accountIdB64: accountIdB64))
    }

    public func setUsernameRegisterOwed(_ name: String?, accountIdB64: String) {
        guard let gate = AccountStoreGate.enterUnlessWiped(accountIdB64) else { return }
        defer { gate.leave() }
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
        guard let gate = AccountStoreGate.enterUnlessWiped(accountIdB64) else { return }
        defer { gate.leave() }
        guard let data = try? JSONEncoder().encode(status) else { return }
        defaults.set(data, forKey: Self.key("status", accountIdB64: accountIdB64))
    }

    // MARK: Notices

    /// The id of the last notice this device has seen; `nil` on a device
    /// that never kept a cursor (it then asks for the last 30 days).
    public func noticeCursor(accountIdB64: String) -> UInt64? {
        (defaults.object(forKey: Self.key("notice-cursor", accountIdB64: accountIdB64)) as? Int64)
            .map { UInt64(max(0, $0)) }
    }

    public func setNoticeCursor(_ cursor: UInt64, accountIdB64: String) {
        guard let gate = AccountStoreGate.enterUnlessWiped(accountIdB64) else { return }
        defer { gate.leave() }
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
        guard let gate = AccountStoreGate.enterUnlessWiped(accountIdB64) else { return }
        defer { gate.leave() }
        var actions = ownActions(accountIdB64: accountIdB64)
        actions[kind.rawValue] = atMs
        defaults.set(actions, forKey: Self.key("own-actions", accountIdB64: accountIdB64))
    }

    /// A marker did its work: it suppresses one notice, not every later one.
    public func clearOwnAction(_ kind: String, accountIdB64: String) {
        // A read-modify-write: after a wipe it would write the rest back.
        guard let gate = AccountStoreGate.enterUnlessWiped(accountIdB64) else { return }
        defer { gate.leave() }
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
/// only to an account whose phrase was saved before it (it had words to
/// replace). (A rotation asks
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
