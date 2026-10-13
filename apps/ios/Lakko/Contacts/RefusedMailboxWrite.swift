// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0074 section 3.5a rule 6 — a refused write re-checks the address once.
//
// A write to a person's mailbox that the Delivery Service refuses (`401`) may
// be a write to the mailbox of a key the person has since rotated away, or a
// write signed with a grant for the other of their two mailboxes. The sender
// then fetches that person's identity bundle through the fail-closed gate
// (which records a proven predecessor), resolves the mailbox and the grant
// again, and retries ONCE when either moved. A fetch that fails, a gate that
// refuses, or a re-check that changes nothing leaves the refusal as it was.
//
//   * `RefusedMailboxWrite`   the decision and the one-retry driver, pure
//                             over injected closures;
//   * `PersonAddressRecheck`  the production re-check: who the person is, the
//                             gated fetch, the resolver reload.

/// The decision and the driver of rule 6.
public enum RefusedMailboxWrite {

    /// Whether `error` is the Delivery Service's refusal of a mailbox write.
    /// Seen through the wrappers the contact and chat layers put around it.
    public static func isRefusal(_ error: Error) -> Bool {
        if case .http(401, _, _)? = error as? DsClientError { return true }
        switch error as? ContactsError {
        case .requestRefused?: return true
        case .wrapped(let inner)?: return isRefusal(inner)
        default: break
        }
        if case .wrapped(_, let inner)? = error as? ChatError { return isRefusal(inner) }
        return false
    }

    /// Whether the write is tried again after the re-check: only when the
    /// address or the authorisation moved. An identical second write would be
    /// refused exactly as the first was.
    public static func retries<Attempt: Equatable>(first: Attempt, rechecked: Attempt?) -> Bool {
        guard let rechecked else { return false }
        return rechecked != first
    }

    /// Write, and on a refusal re-check and write once more.
    ///
    /// - Parameters:
    ///   - resolve: the mailbox and the authorisation of the write, read from
    ///     the CURRENT resolver state (called before the first write and again
    ///     after the re-check).
    ///   - refused: whether an error of `publish` is a refusal (the default is
    ///     ``isRefusal(_:)``; a caller that tries several keys passes its own).
    ///   - recheck: the gated identity fetch. Never throws: a failure simply
    ///     changes nothing.
    ///   - publish: one write.
    /// - Throws: the FIRST refusal when the re-check changed nothing; whatever
    ///   the second write throws otherwise.
    public static func write<Attempt: Equatable, Output>(
        resolve: () throws -> Attempt,
        refused: (Error) -> Bool = RefusedMailboxWrite.isRefusal,
        recheck: () async -> Void,
        publish: (Attempt) async throws -> Output
    ) async throws -> Output {
        let first = try resolve()
        do {
            return try await publish(first)
        } catch let error where refused(error) {
            await recheck()
            let second = try? resolve()
            guard retries(first: first, rechecked: second), let second else { throw error }
            return try await publish(second)
        }
    }
}

/// One attempt at a mailbox write: where it goes and how it is signed.
public struct MailboxWriteAttempt: Equatable {
    public let mailbox: Data
    public let auth: DsMailboxWriteAuth?

    public init(mailbox: Data, auth: DsMailboxWriteAuth?) {
        self.mailbox = mailbox
        self.auth = auth
    }
}

/// One attempt at a sweep Welcome: where it goes and every key tried.
public struct SweepWelcomeAttempt: Equatable {
    public let mailbox: Data
    public let candidates: [DsMailboxWriteAuth]

    public init(mailbox: Data, candidates: [DsMailboxWriteAuth]) {
        self.mailbox = mailbox
        self.candidates = candidates
    }
}

/// The production re-check of rule 6.
@MainActor
public enum PersonAddressRecheck {

    /// A person is re-checked at most once per this interval, however many
    /// writes to them are refused (a Welcome that stays owed is retried on
    /// every poll). Chosen, not measured.
    public static let minIntervalMs: Int64 = 5 * 60 * 1000

    /// Whether a person last re-checked at `lastMs` is due at `nowMs`. A
    /// clock that stepped back re-checks.
    public nonisolated static func due(lastMs: Int64?, nowMs: Int64) -> Bool {
        guard let lastMs else { return true }
        return nowMs - lastMs >= minIntervalMs || nowMs < lastMs
    }

    /// The account id of the person holding any of `keys`, from what this
    /// device knows about them: a contact row first, then a received profile.
    /// `nil` when neither names one (the bundle cannot be fetched then).
    public nonisolated static func accountId(
        ofPersonWithKeys keys: [Data],
        contacts: [(key: Data, accountId: Data?)],
        profileAccountId: (Data) -> Data?
    ) -> Data? {
        for key in keys {
            if let id = contacts.first(where: { $0.key == key && $0.accountId?.count == 16 })?.accountId {
                return id
            }
        }
        for key in keys {
            if let id = profileAccountId(key), id.count == 16 { return id }
        }
        return nil
    }

    private static var lastCheckedMs: [Data: Int64] = [:]

    /// The gated fetch, replaceable in tests.
    static var fetch: @MainActor (_ accountId: Data) async throws -> Void = { accountId in
        _ = try await ContactsClient().resolveVerifiedBundle(accountId: accountId)
    }

    /// Forget the throttle (logout, the account wipe, tests).
    public static func reset() {
        lastCheckedMs = [:]
    }

    /// Fetch the identity bundle of the person holding `accountKey` through
    /// the fail-closed gate and reload the resolver. Does nothing for this
    /// account's own key, for a person with no known account id, and for a
    /// person re-checked less than ``minIntervalMs`` ago.
    public static func recheck(accountKey: Data, accountIdHint: Data? = nil) async {
        let reach = ReachCenter.shared
        guard !reach.keyResolver.isOwn(accountKey), accountKey != reach.ownAccountKey else { return }
        let person = reach.currentAccountKey(of: accountKey)
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        guard due(lastMs: lastCheckedMs[person], nowMs: now) else { return }
        let keys = reach.keyResolver.allKeys(of: accountKey)
        let accountId = accountIdHint.flatMap { $0.count == 16 ? $0 : nil } ?? Self.accountId(
            ofPersonWithKeys: keys,
            contacts: AcceptedContactsStore.load().map { ($0.request.fromPubkey, $0.request.fromAccountId) },
            profileAccountId: { ProfileDirectory.shared.accountId(signatureKeyB64: $0.base64EncodedString()) })
        guard let accountId else { return }
        lastCheckedMs[person] = now
        do {
            try await fetch(accountId)
        } catch {
            // A failed fetch or a refusal of the gate changes nothing: the
            // first refusal stands (an unproven change is never resolved).
            NSLog("ADR-0074 3.5a: the address re-check after a refused write did not complete: \(type(of: error))")
        }
        reach.reloadKeyResolver()
    }

    /// ``recheck(accountKey:accountIdHint:)`` for a write addressed by its
    /// mailbox alone: the person is whoever this device holds a grant or a
    /// delegation for at that mailbox.
    public static func recheck(mailbox pseudonym: Data) async {
        guard let person = ReachCenter.shared.accountKey(forMailbox: pseudonym) else { return }
        await recheck(accountKey: person)
    }
}
