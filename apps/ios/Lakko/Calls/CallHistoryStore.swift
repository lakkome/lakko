// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import CryptoKit
import Foundation

/// The flat, cross-conversation call index the Calls screen reads.
///
/// `docs/ux/navigation.md` section 6.2. The screen must be an INDEX, not a
/// scan: a call belongs to a conversation, so the tempting place to keep it is
/// inside that conversation's transcript — and then the Calls screen costs a
/// walk of every transcript on every open, which gets worse with use. So a row
/// is written here at the same moment the transcript row is written, and the
/// screen reads only this.
///
/// ## Where it is stored
///
/// Per account, as an AES-256-GCM envelope under a Keychain-resident wrap key
/// — the ``ChannelLogStore`` posture, reached through
/// ``CallHistoryBacking``.
///
/// An earlier draft kept it as plain JSON in `UserDefaults`, on the reasoning
/// that the entries hold no key material and that ``GroupCallStore`` already
/// keeps a group id and a call start time that way. That reasoning misses what
/// the index IS once the rows are gathered into one place: a flat,
/// time-ordered record of which conversations this person called and when,
/// which is the social graph over time — the thing invariant #5 keeps off the
/// server. The transcript row this mirrors already lives in the ENCRYPTED
/// message log, so leaving the index in the clear would protect one copy of a
/// fact and not the other. Android files the same index in
/// `EncryptedSharedPreferences`, and two clients disagreeing about how hard
/// the same fact is to read is not a platform difference.
///
/// ## Lifetime
///
/// Process-scoped and bound to one account, like ``ProfileDirectory``: the
/// writer is the call host's record port, which is built once at the
/// composition root and outlives any screen. `forgetDevice` clears it.
@MainActor
public final class CallHistoryStore: ObservableObject {

    /// The one instance. The call hosts write to it from their live wiring and
    /// the Calls screen observes it, so a call that has just ended is on the
    /// screen without anything having to poll.
    public static let shared = CallHistoryStore()

    /// The bound account's calls, newest first and already capped.
    @Published public private(set) var entries: [CallHistoryEntry] = []

    private let backing: CallHistoryBacking
    private var boundAccountIdB64: String?

    public init(backing: CallHistoryBacking = SealedCallHistoryBacking()) {
        self.backing = backing
    }

    /// The backing this store writes through, so a test can assert that the
    /// ordinary one seals rather than having to reach into the file system to
    /// find out.
    var backingForTesting: CallHistoryBacking { backing }

    /// Attach to one account and load its index. Idempotent for the same
    /// account, so a relaunch that restored the session does not re-read on
    /// every evaluation.
    public func bind(accountIdB64: String) {
        guard boundAccountIdB64 != accountIdB64 else { return }
        boundAccountIdB64 = accountIdB64
        entries = CallHistory.ordered(Self.load(backing: backing, accountIdB64: accountIdB64))
    }

    /// File one finished call.
    ///
    /// Ignored while nothing is bound: a call cannot finish before a session
    /// exists, so a write with no account is a bug rather than a case to
    /// guess at, and guessing would file the row under the wrong person.
    public func record(_ entry: CallHistoryEntry) {
        guard let accountIdB64 = boundAccountIdB64 else { return }
        var next = entries.filter { $0.id != entry.id }
        next.append(entry)
        entries = CallHistory.ordered(next)
        Self.save(backing: backing, accountIdB64: accountIdB64, entries: entries)
    }

    /// Forget this device's calls. The transcript rows in each conversation are
    /// untouched — they are the person's own history and the wipe that removes
    /// them is the message log's.
    public func clear() {
        entries = []
        guard let accountIdB64 = boundAccountIdB64 else { return }
        backing.remove(accountIdB64: accountIdB64)
    }

    /// Release the account without touching what is stored (logout).
    public func unbind() {
        boundAccountIdB64 = nil
        entries = []
    }

    // MARK: - Storage

    /// An undecodable blob reads as no calls rather than as a failure: the
    /// index is a convenience over history that also lives in the
    /// conversations, so losing it costs a screen, never a record.
    static func load(backing: CallHistoryBacking, accountIdB64: String) -> [CallHistoryEntry] {
        guard let data = backing.load(accountIdB64: accountIdB64),
              let decoded = try? JSONDecoder().decode([CallHistoryEntry].self, from: data)
        else { return [] }
        return decoded
    }

    static func save(
        backing: CallHistoryBacking,
        accountIdB64: String,
        entries: [CallHistoryEntry]
    ) {
        guard !entries.isEmpty else {
            backing.remove(accountIdB64: accountIdB64)
            return
        }
        guard let data = try? JSONEncoder().encode(entries) else { return }
        backing.save(accountIdB64: accountIdB64, data: data)
    }
}
// MARK: - Backing

/// Where one account's index actually lives.
///
/// A protocol rather than a concrete store so the index can be kept sealed on
/// a device and in memory in a test, without the test reaching for a Keychain
/// it has no entitlement for.
public protocol CallHistoryBacking: Sendable {
    /// The stored bytes for this account, or `nil` when there are none.
    func load(accountIdB64: String) -> Data?
    /// Replace this account's bytes.
    func save(accountIdB64: String, data: Data)
    /// Erase this account's bytes.
    func remove(accountIdB64: String)
}

/// The production backing: one AES-256-GCM envelope per account under a
/// Keychain-resident wrap key, the ``ChannelLogStore`` posture.
///
/// **Why sealed, when the entries hold no key material.** The index is a flat,
/// time-ordered record of which conversations this person called and when —
/// the social graph, over time, which is exactly what invariant #5 keeps off
/// the server. Leaving it in the clear on the device while the transcript row
/// it mirrors sits in the encrypted message log would be protecting the same
/// fact in one place and not the other; Android files the same index in
/// `EncryptedSharedPreferences`, and two clients disagreeing about how hard a
/// fact is to read is not a platform difference.
public struct SealedCallHistoryBacking: CallHistoryBacking {

    /// Keychain service tag for the AES wrap key. Versioned so a later
    /// rotation has somewhere to go without colliding with v1 entries.
    public static let wrapKeyService: String = "me.lakko.call-history-wrap.v1"

    /// One wrap key per app installation, not per account; per-account
    /// isolation comes from the file path, as for ``ChannelLogStore``.
    private static let wrapKeyAccount: String = "wrap-key"

    /// Container subdirectory holding the per-account envelopes.
    private static let subdir: String = "lakko-call-history"

    public init() {}

    public func load(accountIdB64: String) -> Data? {
        let candidates = readCandidates(accountIdB64: accountIdB64)
        guard !candidates.isEmpty else { return nil }
        guard let keyBytes = KeychainWrapKey.read(
            service: Self.wrapKeyService, account: Self.wrapKeyAccount) else {
            NSLog("call history: wrap key unavailable")
            return nil
        }
        let wrapKey = SymmetricKey(data: keyBytes)
        return SharedStorage.firstReadable(candidates) { url -> Data? in
            guard let combined = try? Data(contentsOf: url) else { return nil }
            do {
                let sealed = try AES.GCM.SealedBox(combined: combined)
                return try AES.GCM.open(sealed, using: wrapKey)
            } catch {
                NSLog("call history: open failed")
                return nil
            }
        }
    }

    public func save(accountIdB64: String, data: Data) {
        guard let target = writeTarget(accountIdB64: accountIdB64) else { return }
        let wrapKey = SymmetricKey(data: KeychainWrapKey.loadOrCreate(
            service: Self.wrapKeyService, account: Self.wrapKeyAccount))
        guard let combined = try? AES.GCM.seal(data, using: wrapKey).combined else { return }
        try? FileManager.default.createDirectory(
            at: target.dir, withIntermediateDirectories: true)
        SharedStorage.excludeFromBackup(target.dir)
        try? combined.write(to: target.url, options: .atomic)
        SharedStorage.excludeFromBackup(target.url)
    }

    public func remove(accountIdB64: String) {
        if let n = containerURL(accountIdB64: accountIdB64) {
            try? FileManager.default.removeItem(at: n)
        }
        if let o = appSupportURL(accountIdB64: accountIdB64) {
            try? FileManager.default.removeItem(at: o)
        }
    }

    // MARK: File layout

    private func fileName(_ accountIdB64: String) -> String {
        "\(accountIdB64.replacingOccurrences(of: "/", with: "_")).bin"
    }

    private func containerDir() -> URL? {
        SharedStorage.containerURL()?.appendingPathComponent(Self.subdir)
    }

    private func containerURL(accountIdB64: String) -> URL? {
        containerDir()?.appendingPathComponent(fileName(accountIdB64))
    }

    private func appSupportDir() -> URL? {
        guard let appSupport = try? FileManager.default.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        ) else { return nil }
        return appSupport.appendingPathComponent(Self.subdir)
    }

    private func appSupportURL(accountIdB64: String) -> URL? {
        appSupportDir()?.appendingPathComponent(fileName(accountIdB64))
    }

    private func readCandidates(accountIdB64: String) -> [URL] {
        let fm = FileManager.default
        var urls: [URL] = []
        if let n = containerURL(accountIdB64: accountIdB64), fm.fileExists(atPath: n.path) {
            urls.append(n)
        }
        if let o = appSupportURL(accountIdB64: accountIdB64), fm.fileExists(atPath: o.path) {
            urls.append(o)
        }
        return urls
    }

    private func writeTarget(accountIdB64: String) -> (dir: URL, url: URL)? {
        if let dir = containerDir() {
            return (dir, dir.appendingPathComponent(fileName(accountIdB64)))
        }
        if let dir = appSupportDir() {
            return (dir, dir.appendingPathComponent(fileName(accountIdB64)))
        }
        return nil
    }
}

/// An in-memory backing, for tests and previews. Nothing reaches a disk or a
/// Keychain, so a test can prove the store's behaviour without an entitlement.
public final class InMemoryCallHistoryBacking: CallHistoryBacking, @unchecked Sendable {
    private let lock = NSLock()
    private var blobs: [String: Data] = [:]

    public init() {}

    public func load(accountIdB64: String) -> Data? {
        lock.lock(); defer { lock.unlock() }
        return blobs[accountIdB64]
    }

    public func save(accountIdB64: String, data: Data) {
        lock.lock(); defer { lock.unlock() }
        blobs[accountIdB64] = data
    }

    public func remove(accountIdB64: String) {
        lock.lock(); defer { lock.unlock() }
        blobs.removeValue(forKey: accountIdB64)
    }
}

