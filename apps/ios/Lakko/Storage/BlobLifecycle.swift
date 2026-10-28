// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import UIKit

// ADR-0065 sections 3 and 6 — the client half of a file's lifetime: the
// blob deletes this device owes (durable, retried), and the monthly keep of
// the files of pinned and saved messages and of the own profile photo.

/// The deletes this account still owes, in order, deduplicated (pure).
struct BlobDeleteList: Equatable, Codable {
    /// A list longer than this drops its oldest entries: an undeleted blob
    /// is reclaimed by the year-long lifetime anyway.
    static let maxEntries = 2000

    private(set) var hexIds: [String] = []

    mutating func add(_ ids: [Data]) {
        for id in ids where id.count == 32 {
            let hex = StorageClient.blobIdHex(id)
            if !hexIds.contains(hex) { hexIds.append(hex) }
        }
        if hexIds.count > Self.maxEntries { hexIds.removeFirst(hexIds.count - Self.maxEntries) }
    }

    mutating func remove(_ hex: String) { hexIds.removeAll { $0 == hex } }

    var isEmpty: Bool { hexIds.isEmpty }
}

/// When each kept set last had its keep sent, and which is due (pure).
struct KeepSchedule: Equatable, Codable {
    /// About once a month (ADR-0065 section 3).
    static let intervalMs: Int64 = 30 * 24 * 60 * 60 * 1000
    /// The most sets one tick sends.
    static let maxSetsPerTick = 4

    /// Key -> the last keep sent (unix ms). A key seen for the first time is
    /// first due at its own point of the coming month (``spreadOffsetMs``),
    /// so a device's sets do not all come due at once.
    private(set) var lastSentMs: [String: Int64] = [:]

    /// A stable offset inside one interval for `key`: FNV-1a (32-bit) over
    /// its UTF-16 code units, modulo the interval — the web client's
    /// `spreadOffsetMs`, so the same key spreads the same way.
    static func spreadOffsetMs(_ key: String) -> Int64 {
        var h: UInt32 = 2_166_136_261
        for unit in key.utf16 {
            h ^= UInt32(unit)
            h = h &* 16_777_619
        }
        return Int64(h) % intervalMs
    }

    /// File keys not seen before: first due `spreadOffsetMs` from now.
    mutating func adopt(_ keys: [String], nowMs: Int64) {
        for key in keys where lastSentMs[key] == nil {
            lastSentMs[key] = nowMs - Self.intervalMs + Self.spreadOffsetMs(key)
        }
    }

    /// The due keys among `keys`, most overdue first, at most `limit`.
    func due(_ keys: [String], nowMs: Int64, limit: Int = KeepSchedule.maxSetsPerTick) -> [String] {
        keys.compactMap { key -> (String, Int64)? in
            guard let last = lastSentMs[key], nowMs - last >= Self.intervalMs else { return nil }
            return (key, last)
        }
        .sorted { $0.1 < $1.1 || ($0.1 == $1.1 && $0.0 < $1.0) }
        .prefix(limit)
        .map(\.0)
    }

    mutating func sent(_ key: String, nowMs: Int64) { lastSentMs[key] = nowMs }

    /// Forget keys no longer held (a conversation left).
    mutating func retain(_ keys: Set<String>) { lastSentMs = lastSentMs.filter { keys.contains($0.key) } }
}

/// The file sets a device keeps alive, per key (pure selection).
enum KeepSelection {
    static let savedKey = "saved"
    static let profileKey = "profile"

    /// A group's pinned files: `pins:<MLS group id, lowercase hex>` (pins are
    /// kept per group), the key web and Android use too (it decides the
    /// first-due offset, so the three must agree).
    static func pinsKey(_ groupIdHex: String) -> String { "pins:\(groupIdHex.lowercased())" }

    /// Per conversation, the blob ids of its messages pinned for everyone
    /// or for me; plus the saved messages' and the own profile photo's.
    static func sets(
        threads: [ChatThread],
        pinnedIds: (_ groupIdHex: String) -> Set<Int64>,
        savedBlobIds: [Data],
        profileBlobIds: [Data]
    ) -> [String: [Data]] {
        var out: [String: [Data]] = [:]
        for thread in threads {
            let pinned = pinnedIds(thread.groupIdHex)
            guard !pinned.isEmpty else { continue }
            let ids = MessageBlobs.ids(
                of: thread.messages.filter { pinned.contains($0.messageId) && !$0.deleted }
                    .flatMap(\.attachments))
            if !ids.isEmpty { out[pinsKey(thread.groupIdHex)] = ids }
        }
        if !savedBlobIds.isEmpty { out[savedKey] = savedBlobIds }
        if !profileBlobIds.isEmpty { out[profileKey] = profileBlobIds }
        return out
    }
}

/// The per-account store of the two (UserDefaults, app-private).
struct BlobLifecycleStore {
    let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) { self.defaults = defaults }

    private func key(_ name: String, _ account: String) -> String { "lakko.blob.\(name).v1.\(account)" }

    func deletes(accountIdB64: String) -> BlobDeleteList {
        defaults.data(forKey: key("delete", accountIdB64))
            .flatMap { try? JSONDecoder().decode(BlobDeleteList.self, from: $0) } ?? BlobDeleteList()
    }

    func setDeletes(_ list: BlobDeleteList, accountIdB64: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        defaults.set(try? JSONEncoder().encode(list), forKey: key("delete", accountIdB64))
    }

    func schedule(accountIdB64: String) -> KeepSchedule {
        defaults.data(forKey: key("keep", accountIdB64))
            .flatMap { try? JSONDecoder().decode(KeepSchedule.self, from: $0) } ?? KeepSchedule()
    }

    func setSchedule(_ schedule: KeepSchedule, accountIdB64: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        defaults.set(try? JSONEncoder().encode(schedule), forKey: key("keep", accountIdB64))
    }

    func clear(accountIdB64: String) {
        defaults.removeObject(forKey: key("delete", accountIdB64))
        defaults.removeObject(forKey: key("keep", accountIdB64))
    }
}

/// The runtime: deletes now (and owed), and the foreground keep tick.
@MainActor
public final class BlobLifecycle {
    public static let shared = BlobLifecycle()

    /// The signed-in account, set by the app shell.
    var accountIdB64: () -> String? = { nil }
    /// The chat repository and the sync runtime the keep selection reads.
    weak var chat: ChatRepository?
    weak var sync: DeviceGroupSyncRepository?
    var store = BlobLifecycleStore()
    var storage: () -> StorageClient = { StorageClient() }
    private var running = false

    /// Delete `ids` (own uploads): written to the owed list first, then sent.
    public func delete(_ ids: [Data]) {
        guard let account = accountIdB64(), !ids.isEmpty else { return }
        var list = store.deletes(accountIdB64: account)
        list.add(ids)
        store.setDeletes(list, accountIdB64: account)
        Task { await sendOwedDeletes() }
    }

    /// Send every owed delete once; the ones that fail stay owed.
    public func sendOwedDeletes() async {
        guard let account = accountIdB64() else { return }
        let client = storage()
        for hex in store.deletes(accountIdB64: account).hexIds {
            guard let id = ChatFolders.bytes(fromHex: hex) else { continue }
            do {
                try await client.deleteBlob(blobId: id)
                var list = store.deletes(accountIdB64: account)
                list.remove(hex)
                store.setDeletes(list, accountIdB64: account)
            } catch {
                NSLog("[blob] WARN a blob delete is still owed; retried later")
            }
        }
    }

    /// One foreground tick: owed deletes, then at most one due keep set.
    public func tick(nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1000)) async {
        guard !running, let account = accountIdB64(), let chat else { return }
        running = true
        defer { running = false }
        await sendOwedDeletes()
        let threads = chat.state.threads
        let sets = KeepSelection.sets(
            threads: threads,
            pinnedIds: { groupIdHex in
                let shared = chat.sharedPins(groupIdHex: groupIdHex).filter { $0.value.pinned }.keys
                return Set(shared).union(chat.personalPins(groupIdHex: groupIdHex).keys)
            },
            savedBlobIds: sync?.savedBlobIds() ?? [],
            profileBlobIds: ProfileDirectory.shared.ownProfile?.avatar.map {
                [$0.still.blobId] + ($0.video.map { [$0.blobId] } ?? [])
            } ?? [])
        var schedule = store.schedule(accountIdB64: account)
        let keys = Array(sets.keys)
        schedule.retain(Set(keys))
        schedule.adopt(keys, nowMs: nowMs)
        let client = storage()
        for due in schedule.due(keys, nowMs: nowMs) {
            guard let ids = sets[due] else { continue }
            var start = 0
            var ok = true
            while start < ids.count {
                let chunk = Array(ids[start..<min(ids.count, start + StorageClient.maxKeepIds)])
                do { try await client.keepBlobs(chunk) } catch { ok = false; break }
                start += StorageClient.maxKeepIds
            }
            if ok { schedule.sent(due, nowMs: nowMs) } else { break }
        }
        store.setSchedule(schedule, accountIdB64: account)
    }

    /// The first tick this long after landing.
    static let firstTickDelayNs: UInt64 = 60_000_000_000
    /// Then one every ten minutes while the app is in front.
    static let tickIntervalNs: UInt64 = 600_000_000_000

    /// The tick loop: 60 s after landing, then every ten minutes, only while
    /// the app is in front (never in the background).
    public func runForegroundLoop() async {
        try? await Task.sleep(nanoseconds: Self.firstTickDelayNs)
        while !Task.isCancelled {
            if UIApplication.shared.applicationState == .active { await tick() }
            try? await Task.sleep(nanoseconds: Self.tickIntervalNs)
        }
    }
}
