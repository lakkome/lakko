// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The contact links (ledger slugs) whose ledger row THIS device revoked and
/// has not deleted yet, per account (ADR-0079 section 4).
///
/// The ledger holds one row per link, and only the device that revokes the
/// link deletes it. The deletion is marked owed before the link is revoked in
/// the synced register, so a crash or an unreachable ledger in between leaves
/// it owed; it is retried after every poll until the ledger answers (a
/// deleted row or "no row", 2xx or 404). In `UserDefaults`, keyed by the
/// account; slugs only (a ledger id is not a secret: the seed is). Cleared by
/// forget device (``AccountLocalDataWipe``).
///
/// Mirror of the owed set in `apps/web/src/contacts/own-links.ts`.
public final class OwedLinkRowDeletionStore {
    public static let shared = OwedLinkRowDeletionStore()

    static let prefix = "me.lakko.contacts.link-row-deletions-owed.v1."

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    static func storageKey(_ accountIdB64: String) -> String { prefix + accountIdB64 }

    public func slugs(accountIdB64: String) -> Set<String> {
        Set(defaults.stringArray(forKey: Self.storageKey(accountIdB64)) ?? [])
    }

    public func isOwed(accountIdB64: String, slug: String) -> Bool {
        slugs(accountIdB64: accountIdB64).contains(slug)
    }

    /// Record (`owed = true`) or clear an owed deletion. Not written once the
    /// account's session has ended (``AccountStoreGate``).
    public func set(accountIdB64: String, slug: String, owed: Bool) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var current = slugs(accountIdB64: accountIdB64)
        guard owed != current.contains(slug) else { return }
        if owed { current.insert(slug) } else { current.remove(slug) }
        if current.isEmpty {
            defaults.removeObject(forKey: Self.storageKey(accountIdB64))
        } else {
            defaults.set(current.sorted(), forKey: Self.storageKey(accountIdB64))
        }
    }

    /// Forget device: drop the account's owed list.
    public static func clear(accountIdB64: String, defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: storageKey(accountIdB64))
    }
}

/// Deletes the ledger row of a contact link this device revoked, exactly once
/// (ADR-0079 section 4).
///
/// A deletion runs only while it is owed (``OwedLinkRowDeletionStore``); a
/// call while the same deletion is running joins it instead of sending a
/// second `DELETE`, and a call once it is no longer owed sends nothing. The
/// owed mark is cleared once the ledger answered (row deleted, or 404: gone,
/// expired, revoked earlier) and kept when the ledger could not be reached.
/// Any other refusal is rethrown and the deletion stays owed.
@MainActor
public final class LinkRowDeleter {
    public static let shared = LinkRowDeleter()

    public enum Outcome: Equatable {
        /// This call (or the call it joined) got the ledger's answer.
        case deleted(RevokeInviteResult)
        /// Not owed any more: another call on this device already got the
        /// ledger's answer. Nothing was sent.
        case alreadyDone
        /// The ledger could not be reached; the deletion stays owed.
        case unreachable
        /// No revocation seed is known for the row; nobody on this device can
        /// delete it (the owed mark is dropped).
        case noKey
    }

    /// The ledger call: `DiscoveryClient.revokeInvite(blobId:signingSeed:)`
    /// in production.
    public typealias Delete = (_ blobId: Data, _ seed: Data) async throws -> RevokeInviteResult

    public let owed: OwedLinkRowDeletionStore
    private var inFlight: [String: Task<Outcome, Error>] = [:]

    public init(owed: OwedLinkRowDeletionStore = .shared) {
        self.owed = owed
    }

    /// Whether a deletion of `slug` is running now.
    public func isRunning(accountIdB64: String, slug: String) -> Bool {
        inFlight[Self.key(accountIdB64, slug)] != nil
    }

    /// Run the owed deletion of the row at `blobId`, once (see the type).
    public func deleteOwed(
        accountIdB64: String, blobId: Data, seed: Data?, delete: @escaping Delete
    ) async throws -> Outcome {
        let slug = base64UrlEncodeNoPad(blobId)
        let key = Self.key(accountIdB64, slug)
        if let running = inFlight[key] { return try await running.value }
        guard owed.isOwed(accountIdB64: accountIdB64, slug: slug) else { return .alreadyDone }
        let owed = self.owed
        let task = Task<Outcome, Error> { @MainActor in
            guard let seed, seed.count == 32 else {
                owed.set(accountIdB64: accountIdB64, slug: slug, owed: false)
                return .noKey
            }
            do {
                let result = try await delete(blobId, seed)
                owed.set(accountIdB64: accountIdB64, slug: slug, owed: false)
                return .deleted(result)
            } catch {
                if Self.isUnreachable(error) { return .unreachable }
                throw error
            }
        }
        inFlight[key] = task
        defer { inFlight[key] = nil }
        return try await task.value
    }

    /// A transport failure: no answer from the ledger. Looks through the
    /// repository wrapper.
    nonisolated static func isUnreachable(_ error: Error) -> Bool {
        switch error {
        case DiscoveryClientError.network: return true
        case is URLError: return true
        case ContactsError.wrapped(let inner): return isUnreachable(inner)
        default: return false
        }
    }

    private static func key(_ accountIdB64: String, _ slug: String) -> String { "\(accountIdB64)|\(slug)" }
}

/// The post-poll sweep of revoked contact links (ADR-0079 section 4).
public enum LinkRowSweep {
    /// A contact link the synced register holds as revoked.
    public struct RevokedLink: Equatable {
        public let blobId: Data
        /// The seed the register carries; empty for a record written before
        /// the 2026-10-17 rework.
        public let recordSeed: Data

        public init(blobId: Data, recordSeed: Data) {
            self.blobId = blobId
            self.recordSeed = recordSeed
        }

        public var slug: String { base64UrlEncodeNoPad(blobId) }
    }

    /// What the sweep did, for tests and logs.
    public struct Report: Equatable {
        public var retried = 0
        public var unreachable = 0
        public var forgotten = 0
    }

    /// 1. Retry every deletion this device owes (it revoked the link).
    /// 2. A link revoked by another device of the account: the revoking device
    ///    deletes the row itself, so this device sends the ledger NOTHING and
    ///    only drops its local traces (`forget`). The one exception is a
    ///    record with no seed, whose row no other device can delete: when this
    ///    device holds its own record of the link with a seed, it takes the
    ///    deletion on (marks it owed and runs it once).
    ///
    /// `ownSeed` reads this device's own record of a link (the minting
    /// device's seed). `forget` drops the local traces of a link whose row is
    /// dealt with; it must be idempotent. Never throws.
    @MainActor
    @discardableResult
    public static func run(
        accountIdB64: String,
        revoked: [RevokedLink],
        ownSeed: (_ slug: String) -> Data?,
        deleter: LinkRowDeleter,
        delete: @escaping LinkRowDeleter.Delete,
        forget: (_ blobId: Data) -> Void
    ) async -> Report {
        var report = Report()
        let bySlug = Dictionary(revoked.map { ($0.slug, $0) }, uniquingKeysWith: { first, _ in first })

        func runOwed(slug: String, blobId: Data) async {
            let recordSeed = bySlug[slug]?.recordSeed ?? Data()
            let seed = recordSeed.count == 32 ? recordSeed : ownSeed(slug)
            report.retried += 1
            do {
                let outcome = try await deleter.deleteOwed(
                    accountIdB64: accountIdB64, blobId: blobId, seed: seed, delete: delete)
                if outcome == .unreachable {
                    report.unreachable += 1
                } else {
                    forget(blobId)
                }
            } catch {
                NSLog("[contacts] WARN owed ledger row deletion refused; kept owed")
            }
        }

        // 1. Owed deletions, sorted for a stable order.
        for slug in deleter.owed.slugs(accountIdB64: accountIdB64).sorted() {
            guard let blobId = base64UrlDecodeNoPad(slug), !blobId.isEmpty else {
                deleter.owed.set(accountIdB64: accountIdB64, slug: slug, owed: false)
                continue
            }
            await runOwed(slug: slug, blobId: blobId)
        }

        // 2. Revoked elsewhere.
        for link in revoked {
            let slug = link.slug
            if deleter.owed.isOwed(accountIdB64: accountIdB64, slug: slug)
                || deleter.isRunning(accountIdB64: accountIdB64, slug: slug) {
                continue
            }
            if link.recordSeed.count != 32, let seed = ownSeed(slug), seed.count == 32 {
                deleter.owed.set(accountIdB64: accountIdB64, slug: slug, owed: true)
                await runOwed(slug: slug, blobId: link.blobId)
                continue
            }
            forget(link.blobId)
            report.forgotten += 1
        }
        return report
    }
}
