// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import Security

// ADR-0066 part 1 — "Was it you?" and taking the account back
// (docs/ux/identity-recovery.md section 18.4).
//
// A device that learns the account's keys were reset elsewhere deletes
// everything the account kept on it AT ONCE, except one small record: what it
// needs to prove to the server that it was a device of the account. With that
// record and the password the account had before the reset, the person can
// take the account back for 30 days.
//
//   * `SignOutWipe`            staged wipe / full wipe, decided purely;
//   * `RecallRecord`           what survives the staged wipe;
//   * `KeychainRecallRecordStore`  where it lives (app-private Keychain);
//   * `RecallWindow` / `RecallAnswers`  the server's answers, mapped;
//   * `RecallScreenState` / `RecallMachine`  the screen's state machine;
//   * `RecallForm`             the form's gate;
//   * the wire bodies of `POST /account/recall/{status,start,finish}`.
//
// `AuthRepository` is the state around them.

// MARK: - Which wipe a sign-out runs

/// The deletion a device runs when the server tells it it is out.
public enum SignOutWipe {

    public enum Plan: Equatable {
        /// Everything the account kept here, EXCEPT the recall record; then
        /// "Was it you?".
        case staged
        /// Everything, the recall record included; then the signed-out or
        /// removed screen.
        case full
    }

    /// - Parameters:
    ///   - cause: `key-generation-superseded` (or the landing rule's stale
    ///     keys) is ``DeviceSignOutCause/keysReset``; `device-revoked` (or
    ///     the DeviceGroup reporting this device out) is
    ///     ``DeviceSignOutCause/deviceRemoved``.
    ///   - holdsAccountSeed: this device holds the account identity seed.
    ///   - recallRecordExists: a recall record for the account is already
    ///     stored (a wipe that was interrupted and runs again).
    ///
    /// A removal changes no keys, so it never keeps a record. A device that
    /// never held the keys has nothing to prove with and keeps nothing.
    public static func plan(
        cause: DeviceSignOutCause,
        holdsAccountSeed: Bool,
        recallRecordExists: Bool
    ) -> Plan {
        switch cause {
        case .deviceRemoved:
            return .full
        case .keysReset:
            return holdsAccountSeed || recallRecordExists ? .staged : .full
        }
    }
}

// MARK: - The recall record

/// The one thing the staged wipe keeps: the account id, the OLD account
/// identity's 32-byte Ed25519 seed (not the 64-byte phrase seed, not the
/// phrase), the username for the screen, and when this device learned of the
/// reset. Nothing else of the account.
public struct RecallRecord: Codable, Equatable {
    /// Standard base64 of the 16-byte account id (the key every per-account
    /// store on this device uses).
    public let accountIdB64: String
    /// The Ed25519 signing seed of the account identity this device held.
    public let oldSeed: Data
    /// The username, for the screen; empty for a guest account.
    public let username: String
    /// Unix milliseconds: when this device learned the keys were reset.
    public let learnedAtMs: Int64

    public static let seedLength = 32
    public static let accountIdLength = 16

    public init(accountIdB64: String, oldSeed: Data, username: String, learnedAtMs: Int64) {
        self.accountIdB64 = accountIdB64
        self.oldSeed = oldSeed
        self.username = username
        self.learnedAtMs = learnedAtMs
    }

    enum CodingKeys: String, CodingKey {
        case accountIdB64 = "account_id"
        case oldSeed = "old_seed"
        case username
        case learnedAtMs = "learned_at_ms"
    }

    /// The 16-byte account id, or `nil` for a record that names none.
    public var accountId: Data? {
        guard let bytes = Data(base64Encoded: accountIdB64), bytes.count == Self.accountIdLength else {
            return nil
        }
        return bytes
    }

    /// A record that can be used: a 16-byte account id and a 32-byte seed.
    public var isWellFormed: Bool {
        accountId != nil && oldSeed.count == Self.seedLength
    }

    /// The device-side end of the window: 30 days after this device learned
    /// (ADR-0066 section 1.1; the server's own window is authoritative while
    /// it answers).
    public static let localWindowMs: Int64 = 30 * 24 * 60 * 60 * 1000

    public var localExpiryMs: Int64 { learnedAtMs + Self.localWindowMs }

    public func locallyExpired(nowMs: Int64) -> Bool { nowMs >= localExpiryMs }
}

/// Where the recall record lives.
public protocol RecallRecordStoring {
    /// The stored record (the newest, should more than one account have
    /// one), or `nil`.
    func load() -> RecallRecord?
    func put(_ record: RecallRecord) throws
    func clear(accountIdB64: String)
}

/// The production store: one generic-password Keychain item per account.
///
///   * `kSecAttrService = "me.lakko.recall-record.v1"`
///   * `kSecAttrAccount = accountIdB64`
///   * `kSecAttrAccessible = AfterFirstUnlockThisDeviceOnly`: device-bound,
///     no iCloud Keychain, the hardware-backed class key, the protection the
///     account identity seed has.
///   * NO `kSecAttrAccessGroup`: the item lands in the app's own Keychain
///     group (the first of the app's `keychain-access-groups`), NOT in the
///     shared App Group the identity seed uses. The notification extension
///     is not entitled to the app's own group, so it cannot read the record:
///     nothing in the background has a use for it.
public struct KeychainRecallRecordStore: RecallRecordStoring {

    public static let serviceTag = "me.lakko.recall-record.v1"

    public init() {}

    public func load() -> RecallRecord? {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: Self.serviceTag,
            kSecReturnData: true,
            kSecReturnAttributes: true,
            kSecMatchLimit: kSecMatchLimitAll,
        ]
        var result: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess,
              let rows = result as? [[String: Any]] else { return nil }
        return rows
            .compactMap { $0[kSecValueData as String] as? Data }
            .compactMap { try? JSONDecoder().decode(RecallRecord.self, from: $0) }
            .filter(\.isWellFormed)
            .max { $0.learnedAtMs < $1.learnedAtMs }
    }

    public func put(_ record: RecallRecord) throws {
        guard record.isWellFormed else { throw RecallRecordStoreError.malformed }
        let data = try JSONEncoder().encode(record)
        clear(accountIdB64: record.accountIdB64)
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: Self.serviceTag,
            kSecAttrAccount: record.accountIdB64,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData: data,
        ]
        let status = SecItemAdd(query as CFDictionary, nil)
        guard status == errSecSuccess else { throw RecallRecordStoreError.keychain(status) }
    }

    public func clear(accountIdB64: String) {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: Self.serviceTag,
            kSecAttrAccount: accountIdB64,
        ]
        _ = SecItemDelete(query as CFDictionary)
    }
}

public enum RecallRecordStoreError: Error, Equatable {
    case malformed
    case keychain(OSStatus)
}

// MARK: - The server's answers

/// What `POST /account/recall/status` says while the account can be taken
/// back from this device.
public struct RecallWindow: Equatable, Sendable {
    public let anchorSeq: UInt64
    /// Until when (unix ms).
    public let expiresAtMs: Int64
    /// When the keys were reset (unix ms).
    public let changedAtMs: Int64
    /// How many password attempts are left before the server ends it.
    public let attemptsRemaining: Int

    public init(anchorSeq: UInt64, expiresAtMs: Int64, changedAtMs: Int64, attemptsRemaining: Int) {
        self.anchorSeq = anchorSeq
        self.expiresAtMs = expiresAtMs
        self.changedAtMs = changedAtMs
        self.attemptsRemaining = attemptsRemaining
    }
}

/// The OLD password did not open the server's answer: the client-side OPAQUE
/// login finish failed. Nothing was sent to `finish`.
public struct RecallWrongPassword: Error, Equatable {
    public init() {}
}

/// Every answer of the three recall routes, mapped.
public enum RecallAnswers {

    /// What the status route came to.
    public enum Status: Equatable {
        case open(RecallWindow)
        /// `401 recall-refused`: the account can no longer be taken back from
        /// this device (no anchor for this key, the window passed, the
        /// attempts were used up).
        case refused
        /// No answer, a limit, a server fault: nothing is known; ask again.
        case unknown
    }

    /// Why a take-back attempt did not land.
    public enum Refusal: Equatable {
        /// `401 recall-refused` from `start` or `finish`.
        case refused
        /// `401 wrong-password` from `finish`, or the local OPAQUE finish.
        case wrongPassword
        /// `429 too-many-attempts`.
        case rateLimited(retryAfterSecs: Int?)
        case offline
        /// `400`, `5xx`, an undecodable answer.
        case failed
    }

    public static func status(of error: Error) -> Status {
        switch refusal(of: error) {
        case .refused: return .refused
        case .wrongPassword, .rateLimited, .offline, .failed: return .unknown
        }
    }

    public static func refusal(of error: Error) -> Refusal {
        if error is RecallWrongPassword { return .wrongPassword }
        switch error as? AuthClientError {
        case .http(let status, _, let body): return refusal(status: status, body: body)
        case .network: return .offline
        default: return .failed
        }
    }

    public static func refusal(status: Int, body: String) -> Refusal {
        let json = (try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any]
        let code = json?["error"] as? String
        switch (status, code) {
        case (401, "wrong-password"): return .wrongPassword
        // ADR-0066 "T5 as built" items 17 and 24 — a statement that verified
        // but carried a wrong time. Nothing was counted and nothing is over:
        // the one retry at the server's time already ran, so this is "not
        // known", NEVER the refusal that destroys the recall record.
        case (401, StatementStale.code): return .failed
        // Everything that is not a wrong password is one identical body.
        case (401, _): return .refused
        case (429, _):
            return .rateLimited(retryAfterSecs: (json?["retry_after_secs"] as? NSNumber)?.intValue)
        default: return .failed
        }
    }
}

/// `DELETE /account` while a recall window is open:
/// `409 {"error":"recall-window-open","until_ms"}`.
public enum RecallWindowOpen {

    /// The unix-ms date the deletion is blocked until, or `nil` when
    /// `status` / `body` is not that refusal.
    public static func untilMs(status: Int, body: String) -> Int64? {
        guard status == 409,
              let json = (try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any],
              json["error"] as? String == "recall-window-open",
              let until = json["until_ms"] as? NSNumber else { return nil }
        return until.int64Value
    }

    public static func untilMs(of error: Error) -> Int64? {
        if case .http(let status, _, let body) = error as? WebauthnClientError {
            return untilMs(status: status, body: body)
        }
        if case .http(let status, _, let body) = error as? AuthClientError {
            return untilMs(status: status, body: body)
        }
        return nil
    }
}

// MARK: - The screen's state machine

/// What the device knows about whether the account can be taken back.
public enum RecallCheck: Equatable {
    /// Asking the server; **No** is disabled, `devices.recall.checking`.
    case checking
    /// Confirmed with the server: the until-date is drawn.
    case open(RecallWindow)
    /// The server could not be asked; **No** is disabled, the offline line.
    case offline
}

public enum RecallStep: Equatable {
    /// "Was it you?" with the two buttons.
    case question
    /// **Yes, it was me**: the confirmation, inside the same screen.
    case confirmYes
    /// **No, take the account back**: the form.
    case form
}

public enum RecallFormStatus: Equatable {
    case idle
    case working
    /// `triesLeft` only when the server said fewer than five are left.
    case wrongPassword(triesLeft: Int?)
    case rateLimited(retryAfterSecs: Int?)
    case offline
    case failed
}

/// The "Was it you?" screen. Driven by the recall record, so it survives a
/// restart.
public struct RecallScreenState: Equatable {
    public let accountIdB64: String
    public let username: String
    public let learnedAtMs: Int64
    public var check: RecallCheck = .checking
    public var step: RecallStep = .question
    public var form: RecallFormStatus = .idle

    public init(record: RecallRecord) {
        self.accountIdB64 = record.accountIdB64
        self.username = record.username
        self.learnedAtMs = record.learnedAtMs
    }

    /// The date the body names: the server's when it is known, else when
    /// this device learned.
    public var resetAtMs: Int64 {
        if case .open(let window) = check { return window.changedAtMs }
        return learnedAtMs
    }

    /// The until-date, drawn only once confirmed with the server.
    public var untilMs: Int64? {
        if case .open(let window) = check { return window.expiresAtMs }
        return nil
    }

    /// Whether **No, take the account back** is enabled.
    public var canTakeBack: Bool {
        if case .open = check { return true }
        return false
    }
}

/// How the recall ended without the account being taken back. Each one
/// destroys the recall record and finishes the wipe; the screen becomes the
/// signed-out screen of UX 15.2.
public enum RecallEnd: Equatable {
    /// **Yes, it was me**, confirmed.
    case answeredYes
    /// The server no longer offers it and the person was not in the form:
    /// nothing says why.
    case closed
    /// The time passed while the form was open.
    case closedInForm
    /// Too many wrong passwords.
    case exhausted
    /// The device's own 30 days passed.
    case expired

    /// The line the signed-out screen adds, or `nil`.
    public var noticeKey: String? {
        switch self {
        case .closedInForm: return "devices.recall.error.closed"
        case .exhausted: return "devices.recall.error.exhausted"
        case .answeredYes, .closed, .expired: return nil
        }
    }
}

/// The transitions, pure.
public enum RecallMachine {

    public enum Transition: Equatable {
        case show(RecallScreenState)
        case end(RecallEnd)
    }

    /// Tries are named only below this many (UX 18.4).
    public static let triesLeftShownBelow = 5

    /// Whether the record's own window passed, or the server's did.
    public static func expired(_ state: RecallScreenState, nowMs: Int64) -> Bool {
        if nowMs >= state.learnedAtMs + RecallRecord.localWindowMs { return true }
        if let until = state.untilMs, nowMs >= until { return true }
        return false
    }

    /// A status answer arrived.
    public static func afterStatus(
        _ state: RecallScreenState, _ answer: RecallAnswers.Status, nowMs: Int64
    ) -> Transition {
        if expired(state, nowMs: nowMs) { return .end(.expired) }
        var next = state
        switch answer {
        case .open(let window):
            if nowMs >= window.expiresAtMs { return .end(state.step == .form ? .closedInForm : .closed) }
            if window.attemptsRemaining <= 0 { return .end(state.step == .form ? .exhausted : .closed) }
            next.check = .open(window)
        case .refused:
            return .end(state.step == .form ? .closedInForm : .closed)
        case .unknown:
            // What was confirmed before stays confirmed; a first check that
            // could not be made shows the offline line.
            if case .open = state.check { break }
            next.check = .offline
        }
        return .show(next)
    }

    /// **Yes, it was me**.
    public static func tapYes(_ state: RecallScreenState) -> RecallScreenState {
        var next = state
        if next.step == .question { next.step = .confirmYes }
        return next
    }

    /// **No, take the account back**: only once the server confirmed it.
    public static func tapNo(_ state: RecallScreenState) -> RecallScreenState {
        var next = state
        if next.step == .question, next.canTakeBack {
            next.step = .form
            next.form = .idle
        }
        return next
    }

    /// `Go back` on the confirmation, `Back` on the form. Not while the
    /// form is working.
    public static func back(_ state: RecallScreenState) -> RecallScreenState {
        var next = state
        guard next.form != .working else { return next }
        next.step = .question
        next.form = .idle
        return next
    }

    /// `Delete and sign in`.
    public static func confirmYes(_ state: RecallScreenState) -> Transition {
        state.step == .confirmYes ? .end(.answeredYes) : .show(state)
    }

    /// The form was submitted; `nil` when it cannot be (not in the form,
    /// already working, the window not confirmed).
    public static func submit(_ state: RecallScreenState) -> RecallScreenState? {
        guard state.step == .form, state.form != .working, state.canTakeBack else { return nil }
        var next = state
        next.form = .working
        return next
    }

    /// The old password was wrong; `status` is the fresh status read after
    /// it.
    public static func afterWrongPassword(
        _ state: RecallScreenState, status: RecallAnswers.Status, nowMs: Int64
    ) -> Transition {
        if expired(state, nowMs: nowMs) { return .end(.expired) }
        var next = state
        switch status {
        case .refused:
            // The attempt that just failed was the last one.
            return .end(.exhausted)
        case .open(let window):
            if window.attemptsRemaining <= 0 { return .end(.exhausted) }
            if nowMs >= window.expiresAtMs { return .end(.closedInForm) }
            next.check = .open(window)
            next.form = .wrongPassword(
                triesLeft: window.attemptsRemaining < triesLeftShownBelow ? window.attemptsRemaining : nil)
        case .unknown:
            next.form = .wrongPassword(triesLeft: nil)
        }
        return .show(next)
    }

    /// `start` or `finish` refused the attempt for anything but a wrong
    /// password (which goes through ``afterWrongPassword(_:status:nowMs:)``).
    public static func afterRefusal(
        _ state: RecallScreenState, _ refusal: RecallAnswers.Refusal
    ) -> Transition {
        var next = state
        switch refusal {
        case .refused: return .end(.closedInForm)
        case .wrongPassword: next.form = .wrongPassword(triesLeft: nil)
        case .rateLimited(let secs): next.form = .rateLimited(retryAfterSecs: secs)
        case .offline: next.form = .offline
        case .failed: next.form = .failed
        }
        return .show(next)
    }
}

/// The take-back form's gate (UX 18.4, "Idle").
public enum RecallForm {

    public enum Problem: Equatable {
        case incomplete
        case tooShort
        case mismatch
    }

    /// Why the form cannot be submitted, or `nil` when it can: the three
    /// fields are filled, the new password passes the sign-up rule and the
    /// two match.
    public static func problem(oldPassword: String, newPassword: String, repeated: String) -> Problem? {
        if oldPassword.isEmpty || newPassword.isEmpty || repeated.isEmpty { return .incomplete }
        if newPassword.count < NewPasswordRule.minLength { return .tooShort }
        if newPassword != repeated { return .mismatch }
        return nil
    }

    public static func canSubmit(
        oldPassword: String, newPassword: String, repeated: String, status: RecallFormStatus, waiting: Bool
    ) -> Bool {
        status != .working && !waiting
            && problem(oldPassword: oldPassword, newPassword: newPassword, repeated: repeated) == nil
    }

    /// Whether the fields and the buttons are disabled.
    public static func locked(_ status: RecallFormStatus) -> Bool { status == .working }
}

/// The catalog keys of the recall surfaces (UX 18.6).
public enum RecallCopy {
    /// The screen's title is the reset screen's.
    public static let title = "devices.signed_out.title"
    public static let body = "devices.recall.body"
    public static let question = "devices.recall.question"
    public static let yes = "devices.recall.yes"
    public static let no = "devices.recall.no"
    public static let until = "devices.recall.until"
    public static let checking = "devices.recall.checking"
    public static let offline = "common.error.offline"
    public static let yesConfirm = "devices.recall.yes.confirm"
    public static let yesDelete = "devices.recall.yes.delete"
    public static let yesBack = "devices.recall.yes.back"
    public static let formTitle = "devices.recall.form.title"
    public static let formLede = "devices.recall.form.lede"
    public static let oldPassword = "devices.recall.form.old_password"
    public static let newPassword = "devices.recall.form.new_password"
    public static let repeatPassword = "devices.recall.form.repeat_password"
    public static let effectKeys = "devices.recall.form.effect_keys"
    public static let effectFactors = "devices.recall.form.effect_factors"
    public static let effectMessages = "devices.recall.form.effect_messages"
    public static let submit = "devices.recall.form.submit"
    public static let working = "devices.recall.form.working"
    public static let wrongPassword = "devices.recall.error.wrong_password"
    public static let triesLeft = "devices.recall.error.tries_left"
    public static let exhausted = "devices.recall.error.exhausted"
    public static let closed = "devices.recall.error.closed"
    public static let rateLimited = "recovery.error.rate_limited"
    public static let failed = "common.error.unavailable"
    public static let done = "devices.recall.done"
    public static let deleteBlocked = "settings.account.delete.blocked"
    public static let back = "common.back"

    public static let all: [String] = [
        title, body, question, yes, no, until, checking, offline, yesConfirm, yesDelete, yesBack,
        formTitle, formLede, oldPassword, newPassword, repeatPassword, effectKeys, effectFactors,
        effectMessages, submit, working, wrongPassword, triesLeft, exhausted, closed, rateLimited,
        failed, done, deleteBlocked, back,
    ]

    /// The line under the first field for a form status, or `nil`.
    public static func formErrorKey(_ status: RecallFormStatus) -> String? {
        switch status {
        case .idle, .working, .rateLimited: return nil
        case .wrongPassword: return wrongPassword
        case .offline: return offline
        case .failed: return failed
        }
    }

    /// The platform's medium date in the interface language (UX 18.8).
    public static func date(ms: Int64, locale: Locale) -> String {
        let formatter = DateFormatter()
        formatter.locale = locale
        formatter.dateStyle = .medium
        formatter.timeStyle = .none
        return formatter.string(from: Date(timeIntervalSince1970: TimeInterval(ms) / 1000))
    }
}

// MARK: - Wire (services/auth, anonymous routes)
//
// `*_b64` fields are URL-safe base64 without padding; the OPAQUE messages are
// standard base64, as on every other route of the service.

struct RecallStatusRequest: Codable, Equatable {
    let statementB64: String
    enum CodingKeys: String, CodingKey { case statementB64 = "statement_b64" }
}

struct RecallStatusResponse: Codable, Equatable {
    let anchorSeq: UInt64
    let expiresAtMs: Int64
    let changedAtMs: Int64
    let attemptsRemaining: Int

    enum CodingKeys: String, CodingKey {
        case anchorSeq = "anchor_seq"
        case expiresAtMs = "expires_at_ms"
        case changedAtMs = "changed_at_ms"
        case attemptsRemaining = "attempts_remaining"
    }

    var window: RecallWindow {
        RecallWindow(
            anchorSeq: anchorSeq, expiresAtMs: expiresAtMs, changedAtMs: changedAtMs,
            attemptsRemaining: attemptsRemaining)
    }
}

struct RecallStartRequest: Codable, Equatable {
    let accountIdB64: String
    let anchorSeq: UInt64
    let statementB64: String
    let loginRequest: String
    let passwordRequest: String
    let recoveryRequest: String

    enum CodingKeys: String, CodingKey {
        case accountIdB64 = "account_id_b64"
        case anchorSeq = "anchor_seq"
        case statementB64 = "statement_b64"
        case loginRequest = "login_request"
        case passwordRequest = "password_request"
        case recoveryRequest = "recovery_request"
    }
}

struct RecallStartResponse: Codable, Equatable {
    let sessionId: String
    let loginResponse: String
    let passwordResponse: String
    let recoveryResponse: String
    let serverNonceB64: String

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case loginResponse = "login_response"
        case passwordResponse = "password_response"
        case recoveryResponse = "recovery_response"
        case serverNonceB64 = "server_nonce_b64"
    }
}

struct RecallFinishRequest: Codable, Equatable {
    let sessionId: String
    let accountIdB64: String
    let anchorSeq: UInt64
    let loginFinish: String
    let passwordUpload: String
    let recoveryUpload: String
    let newSignaturePubkeyB64: String
    /// Absent, never null, when the session is to name no device yet.
    let deviceIdB64: String?
    let statementB64: String

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case accountIdB64 = "account_id_b64"
        case anchorSeq = "anchor_seq"
        case loginFinish = "login_finish"
        case passwordUpload = "password_upload"
        case recoveryUpload = "recovery_upload"
        case newSignaturePubkeyB64 = "new_signature_pubkey_b64"
        case deviceIdB64 = "device_id_b64"
        case statementB64 = "statement_b64"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(sessionId, forKey: .sessionId)
        try container.encode(accountIdB64, forKey: .accountIdB64)
        try container.encode(anchorSeq, forKey: .anchorSeq)
        try container.encode(loginFinish, forKey: .loginFinish)
        try container.encode(passwordUpload, forKey: .passwordUpload)
        try container.encode(recoveryUpload, forKey: .recoveryUpload)
        try container.encode(newSignaturePubkeyB64, forKey: .newSignaturePubkeyB64)
        try container.encodeIfPresent(deviceIdB64, forKey: .deviceIdB64)
        try container.encode(statementB64, forKey: .statementB64)
    }
}

struct RecallFinishResponse: Codable, Equatable, SessionPairCarrying {
    let keyResetAttestB64: String
    let keyGeneration: UInt64
    let sessionToken: String
    let sessionExpiresAtMs: Int64
    let refreshToken: String
    let refreshExpiresAtMs: Int64
    let freshGrant: String?
    let freshGrantExpiresAtMs: Int64?

    enum CodingKeys: String, CodingKey {
        case keyResetAttestB64 = "key_reset_attest_b64"
        case keyGeneration = "key_generation"
        case sessionToken = "session_token"
        case sessionExpiresAtMs = "session_expires_at_ms"
        case refreshToken = "refresh_token"
        case refreshExpiresAtMs = "refresh_expires_at_ms"
        case freshGrant = "fresh_grant"
        case freshGrantExpiresAtMs = "fresh_grant_expires_at_ms"
    }
}

/// One take-back attempt: what the client sends through `start` and `finish`.
public struct RecallAttempt {
    public let record: RecallRecord
    public let anchorSeq: UInt64
    public let oldPassword: Data
    public let newPassword: Data
    /// The 64-byte BIP39 seed of the NEW phrase.
    public let newSeed: Data
    /// The NEW account identity's Ed25519 public key.
    public let newSignaturePubkey: Data
    /// This device's id, or `nil` to bind the session after the landing.
    public let deviceId: Data?

    public init(
        record: RecallRecord, anchorSeq: UInt64, oldPassword: Data, newPassword: Data,
        newSeed: Data, newSignaturePubkey: Data, deviceId: Data?
    ) {
        self.record = record
        self.anchorSeq = anchorSeq
        self.oldPassword = oldPassword
        self.newPassword = newPassword
        self.newSeed = newSeed
        self.newSignaturePubkey = newSignaturePubkey
        self.deviceId = deviceId
    }
}

/// What a recall that landed returns: the reset's answer plus a fresh-auth
/// grant.
public struct RecallLanded {
    public let reset: KeyResetResult
    public let freshGrant: FreshAuthGrant?

    public init(reset: KeyResetResult, freshGrant: FreshAuthGrant?) {
        self.reset = reset
        self.freshGrant = freshGrant
    }
}

/// The two server operations of the recall, injectable so the flow runs
/// under a scripted server in tests.
public struct RecallOperations {
    /// `POST /account/recall/status`.
    public var status: (_ record: RecallRecord) async throws -> RecallWindow
    /// `POST /account/recall/{start,finish}`. Throws ``RecallWrongPassword``
    /// when the old password did not open `start`'s answer.
    public var takeBack: (_ attempt: RecallAttempt) async throws -> RecallLanded

    public init(
        status: @escaping (_ record: RecallRecord) async throws -> RecallWindow,
        takeBack: @escaping (_ attempt: RecallAttempt) async throws -> RecallLanded
    ) {
        self.status = status
        self.takeBack = takeBack
    }

    public static func live(_ factory: @escaping () -> AuthClient) -> RecallOperations {
        RecallOperations(
            status: { record in try await factory().recallStatus(record: record) },
            takeBack: { attempt in try await factory().recallAccount(attempt) }
        )
    }
}
