// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore
import SwiftUI

// ADR-0098 (`docs/ux/deleted-account.md`) — what this device knows about
// whether a person's account still exists.
//
// A person is learned to be gone two ways: a verified account-deleted
// statement their device posted in a conversation (final), and the
// directory's answer to `GET /discovery/identity/{account_id}` (a `404` is
// `absent`, reversible by a later `samePerson`). The rules that combine them
// are the core's (`accountDeletionStatus`, `mayShowUsername`,
// `directoryCheckDue`); this file keeps the per-person facts they read, keyed
// on the person's ACCOUNT key (base64), and decides nothing itself.
//
// The facts are per account on this device, in `UserDefaults` behind the
// account store gate, and go with the account (``AccountLocalDataWipe``).

/// The facts kept for one person.
public struct PersonDeletionRecord: Codable, Equatable, Sendable {
    /// A statement from this person verified in one of our conversations.
    public var statementVerified = false
    /// The latest recorded directory answer: `absent`, `samePerson`,
    /// `otherKey`.
    public var lastAnswer: String?
    /// When that answer arrived (this device's clock).
    public var lastAnswerAtMs: UInt64?
    /// When the directory last confirmed the account exists with this key.
    public var lastSamePersonAtMs: UInt64?
    /// When this device last asked the directory about this person.
    public var lastAttemptAtMs: UInt64?
    /// The person's account id (base64), when known.
    public var accountIdB64: String?
    /// The groups (hex) a statement from this person was verified in.
    public var statementGroups: [String] = []
    /// The groups (hex) where this device has proposed their removal.
    public var removalProposed: [String] = []

    public init() {}

    /// The recorded directory answer as the core's type.
    public var directoryAnswer: FfiDirectoryAnswer? {
        switch lastAnswer {
        case "absent": return .absent
        case "samePerson": return .samePerson
        case "otherKey": return .otherKey
        default: return nil
        }
    }
}

/// The pure transitions of a record and the classification of a directory
/// answer, written once so the tests run them without a network.
public enum DeletedAccountRules {

    static func answerName(_ answer: FfiDirectoryAnswer) -> String {
        switch answer {
        case .absent: return "absent"
        case .samePerson: return "samePerson"
        case .otherKey: return "otherKey"
        }
    }

    /// The record after a verified statement in `groupIdHex`. Final.
    public static func withStatement(_ r: PersonDeletionRecord, groupIdHex: String?) -> PersonDeletionRecord {
        var next = r
        next.statementVerified = true
        if let groupIdHex, !next.statementGroups.contains(groupIdHex) {
            next.statementGroups.append(groupIdHex)
        }
        return next
    }

    /// The record after a complete directory answer at `nowMs`.
    public static func withAnswer(
        _ r: PersonDeletionRecord, _ answer: FfiDirectoryAnswer, nowMs: UInt64
    ) -> PersonDeletionRecord {
        var next = r
        next.lastAnswer = answerName(answer)
        next.lastAnswerAtMs = nowMs
        if answer == .samePerson { next.lastSamePersonAtMs = nowMs }
        return next
    }

    /// The person's status over every key they are known by: a statement
    /// under any of them is final; otherwise the newest recorded answer.
    public static func status(_ records: [PersonDeletionRecord]) -> FfiAccountStatus {
        if records.contains(where: { $0.statementVerified }) {
            return accountDeletionStatus(statementVerified: true, lastDirectoryAnswer: nil)
        }
        let newest = records
            .filter { $0.directoryAnswer != nil }
            .max { ($0.lastAnswerAtMs ?? 0) < ($1.lastAnswerAtMs ?? 0) }
        return accountDeletionStatus(statementVerified: false, lastDirectoryAnswer: newest?.directoryAnswer)
    }

    /// Whether a username may be shown beside the person at `nowMs`.
    public static func mayShowUsername(_ records: [PersonDeletionRecord], nowMs: UInt64) -> Bool {
        let confirmed = records.compactMap(\.lastSamePersonAtMs).max()
        return LakkoCore.mayShowUsername(status: status(records), confirmedAtMs: confirmed, nowMs: nowMs)
    }

    /// Whether to ask the directory about the person now.
    public static func checkDue(_ r: PersonDeletionRecord, nowMs: UInt64) -> Bool {
        directoryCheckDue(
            statementVerified: r.statementVerified, lastAnswerAtMs: r.lastAnswerAtMs,
            lastAttemptAtMs: r.lastAttemptAtMs, nowMs: nowMs)
    }

    /// Classify a complete `GET /discovery/identity` answer for a person this
    /// device holds under `heldKey`: `nil` (a `404`) is `absent`; the held
    /// key, or a conversation link that verifies naming it, is `samePerson`;
    /// anything else `otherKey`.
    public static func classify(bundle: IdentityFetchResponse?, heldKey: Data) -> FfiDirectoryAnswer {
        guard let bundle else { return .absent }
        guard let served = Data(base64Encoded: bundle.signaturePubkey) else { return .otherKey }
        if served == heldKey { return .samePerson }
        if let linkB64 = bundle.conversationLink, let link = Data(base64Encoded: linkB64),
           let named = try? verifyConversationLink(link: link, trustedCurrentKey: served),
           named == heldKey {
            return .samePerson
        }
        return .otherKey
    }

    /// The groups where a statement was verified and removal is not proposed.
    public static func removalsOwed(_ r: PersonDeletionRecord) -> [String] {
        guard r.statementVerified else { return [] }
        return r.statementGroups.filter { !r.removalProposed.contains($0) }
    }
}

/// The records of one account in `UserDefaults`.
public final class DeletedAccountStore {
    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func load(accountIdB64: String) -> [String: PersonDeletionRecord] {
        guard let data = defaults.data(forKey: Self.key(accountIdB64)),
              let rows = try? JSONDecoder().decode([String: PersonDeletionRecord].self, from: data)
        else { return [:] }
        return rows
    }

    public func save(accountIdB64: String, _ rows: [String: PersonDeletionRecord]) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        guard let data = try? JSONEncoder().encode(rows) else { return }
        defaults.set(data, forKey: Self.key(accountIdB64))
    }

    public static func clear(accountIdB64: String, defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: key(accountIdB64))
    }

    static func key(_ accountIdB64: String) -> String {
        "deleted_accounts.\(accountIdB64)"
    }
}

/// The app-scoped holder of the records: who is deleted, whose username may
/// be shown, and the directory checks for people on screen.
@MainActor
public final class DeletedAccounts: ObservableObject {
    public static let shared = DeletedAccounts()

    /// Bumped whenever a record changes.
    @Published public private(set) var revision: UInt64 = 0

    private let store: DeletedAccountStore
    private(set) var accountIdB64: String?
    private var records: [String: PersonDeletionRecord] = [:]
    /// People whose directory check is in flight (one per person).
    private var inFlight: Set<String> = []

    /// This device's clock in milliseconds. Tests set their own.
    var nowMs: () -> UInt64 = { UInt64(max(0, Date().timeIntervalSince1970 * 1000)) }
    /// `GET /discovery/identity/{id}`: the bundle, `nil` on a `404`, a throw
    /// on anything that is not an answer. Tests set their own.
    var fetchIdentity: (Data) async throws -> IdentityFetchResponse? = { accountId in
        try await DiscoveryClient().identityFetch(accountId: accountId)
    }
    /// The account keys this device holds for an account id (contacts, member
    /// profiles), for an answer from an identity fetch made elsewhere.
    var heldKeysForAccountId: (Data) -> [Data] = { accountId in
        DeletedAccounts.heldKeys(accountId: accountId)
    }
    /// A verified statement made someone deleted (the chat layer proposes the
    /// group removals and refreshes the background name mirror).
    var onStatementRecorded: ((_ personKeyB64: String) -> Void)?

    init(store: DeletedAccountStore = DeletedAccountStore()) {
        self.store = store
    }

    // MARK: - Account lifecycle

    public func activate(accountIdB64: String) {
        guard self.accountIdB64 != accountIdB64 else { return }
        self.accountIdB64 = accountIdB64
        records = store.load(accountIdB64: accountIdB64)
        inFlight = []
        Self.installObserver()
        bump()
    }

    public func deactivate() {
        accountIdB64 = nil
        records = [:]
        inFlight = []
        bump()
    }

    public func forget(accountIdB64: String) {
        DeletedAccountStore.clear(accountIdB64: accountIdB64)
        if self.accountIdB64 == accountIdB64 {
            records = [:]
            inFlight = []
            bump()
        }
    }

    // MARK: - Reading

    public func record(personKeyB64: String) -> PersonDeletionRecord? {
        records[personKeyB64]
    }

    /// The person's status over every key they are known by.
    public func status(keysB64: [String]) -> FfiAccountStatus {
        DeletedAccountRules.status(keysB64.compactMap { records[$0] })
    }

    public func isDeleted(keysB64: [String]) -> Bool {
        status(keysB64: keysB64) == .deleted
    }

    public func isDeleted(personKey: Data) -> Bool {
        isDeleted(keysB64: [base64Encode(personKey)])
    }

    /// Whether a `@username` may be shown beside the person now.
    public func mayShowUsername(keysB64: [String]) -> Bool {
        DeletedAccountRules.mayShowUsername(keysB64.compactMap { records[$0] }, nowMs: nowMs())
    }

    /// The people with a verified statement in `groupIdHex` whose removal is
    /// not proposed yet.
    public func removalsOwed(groupIdHex: String) -> [String] {
        records.compactMap { key, r in
            DeletedAccountRules.removalsOwed(r).contains(groupIdHex) ? key : nil
        }
    }

    // MARK: - Writing

    /// A statement from `personKeyB64` verified in `groupIdHex`. Final.
    public func noteStatement(personKeyB64: String, groupIdHex: String?) {
        guard accountIdB64 != nil else { return }
        let before = records[personKeyB64] ?? PersonDeletionRecord()
        let after = DeletedAccountRules.withStatement(before, groupIdHex: groupIdHex)
        guard after != before else { return }
        write(personKeyB64, after)
        onStatementRecorded?(personKeyB64)
    }

    /// A complete directory answer about `personKeyB64`.
    public func noteAnswer(personKeyB64: String, accountIdB64 personAccountIdB64: String?, _ answer: FfiDirectoryAnswer) {
        guard accountIdB64 != nil else { return }
        var r = DeletedAccountRules.withAnswer(records[personKeyB64] ?? PersonDeletionRecord(), answer, nowMs: nowMs())
        if let personAccountIdB64 { r.accountIdB64 = personAccountIdB64 }
        write(personKeyB64, r)
    }

    /// This device proposed the removal of `personKeyB64` in `groupIdHex`.
    public func noteRemovalProposed(personKeyB64: String, groupIdHex: String) {
        guard var r = records[personKeyB64], !r.removalProposed.contains(groupIdHex) else { return }
        r.removalProposed.append(groupIdHex)
        write(personKeyB64, r)
    }

    private func write(_ personKeyB64: String, _ r: PersonDeletionRecord) {
        guard let accountIdB64 else { return }
        records[personKeyB64] = r
        store.save(accountIdB64: accountIdB64, records)
        bump()
    }

    private func bump() {
        revision &+= 1
    }

    // MARK: - The pull (a person on screen)

    /// Ask the directory about the person holding `personKey`, when the rule
    /// says a check is due. For a person on screen only. At most one request
    /// in flight per person; the attempt is recorded before asking; a network
    /// failure records nothing else.
    public func checkIfDue(personKey: Data, accountId: Data?) {
        let keyB64 = base64Encode(personKey)
        guard accountIdB64 != nil, let accountId, accountId.count == DiscoveryClient.accountIdLength,
              !inFlight.contains(keyB64) else { return }
        var r = records[keyB64] ?? PersonDeletionRecord()
        let now = nowMs()
        guard DeletedAccountRules.checkDue(r, nowMs: now) else { return }
        r.lastAttemptAtMs = now
        r.accountIdB64 = base64Encode(accountId)
        write(keyB64, r)
        inFlight.insert(keyB64)
        let fetch = fetchIdentity
        let bound = accountIdB64
        Task { @MainActor [weak self] in
            let answer: FfiDirectoryAnswer?
            do {
                answer = DeletedAccountRules.classify(bundle: try await fetch(accountId), heldKey: personKey)
            } catch {
                answer = nil
            }
            guard let self else { return }
            self.inFlight.remove(keyB64)
            guard self.accountIdB64 == bound, let answer else { return }
            self.noteAnswer(personKeyB64: keyB64, accountIdB64: base64Encode(accountId), answer)
        }
    }

    /// An answer from an identity fetch made anywhere in the app: recorded
    /// for every key this device holds for that account id.
    func noteFetched(accountId: Data, bundle: IdentityFetchResponse?) {
        guard accountIdB64 != nil else { return }
        var keys = heldKeysForAccountId(accountId)
        let idB64 = base64Encode(accountId)
        for (key, r) in records where r.accountIdB64 == idB64 {
            if let raw = Data(base64Encoded: key), !keys.contains(raw) { keys.append(raw) }
        }
        for key in keys {
            noteAnswer(
                personKeyB64: base64Encode(key), accountIdB64: idB64,
                DeletedAccountRules.classify(bundle: bundle, heldKey: key))
        }
    }

    private static var observerInstalled = false

    /// Every complete identity answer reaches ``noteFetched(accountId:bundle:)``.
    static func installObserver() {
        guard !observerInstalled else { return }
        observerInstalled = true
        DiscoveryClient.identityAnswerObserver = { accountId, bundle in
            Task { @MainActor in
                DeletedAccounts.shared.noteFetched(accountId: accountId, bundle: bundle)
            }
        }
    }

    /// The account id this device knows for the person holding `personKey`:
    /// the contact record's, else the one their own profile carries.
    static func accountId(personKey: Data) -> Data? {
        if let contact = AcceptedContactsStore.load().first(where: { $0.request.fromPubkey == personKey }),
           let id = contact.request.fromAccountId {
            return id
        }
        return ProfileDirectory.shared.accountId(signatureKeyB64: base64Encode(personKey))
    }

    /// The keys this device holds for `accountId`: contacts, pending
    /// contacts, and member profiles that name it.
    static func heldKeys(accountId: Data) -> [Data] {
        var keys: [Data] = []
        for contact in AcceptedContactsStore.load() where contact.request.fromAccountId == accountId {
            keys.append(contact.request.fromPubkey)
        }
        for entry in ProfileDirectory.shared.profilesWithAccountId() where entry.accountId == accountId {
            if let key = Data(base64Encoded: entry.signatureKeyB64), !keys.contains(key) { keys.append(key) }
        }
        return keys
    }
}

/// `docs/ux/deleted-account.md` section 2 — ask whether the person drawn here
/// still has an account, when a check is due. For the surfaces the ADR names
/// (a contacts row, an open one-to-one, a person card, a member row).
struct ChecksAccountExists: ViewModifier {
    let personKey: Data?
    let accountId: Data?

    func body(content: Content) -> some View {
        content.task(id: personKey) {
            guard let personKey else { return }
            DeletedAccounts.shared.checkIfDue(
                personKey: personKey, accountId: accountId ?? DeletedAccounts.accountId(personKey: personKey))
        }
    }
}

extension View {
    /// See ``ChecksAccountExists``.
    func checksAccountExists(personKey: Data?, accountId: Data? = nil) -> some View {
        modifier(ChecksAccountExists(personKey: personKey, accountId: accountId))
    }
}
