// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

// ADR-0101 section 7 — a channel this account took back with its recovery
// key after a key reset, until its PublisherGroup is founded again and its
// type is known again; and the returned invitations shown above the chat
// list.
//
// The reset lost everything the owner's devices kept about the channel: the
// admission seeds, the read key, the type. Who reads the channel is still on
// the channels service. So at recovery the owner's device records the
// service's highest subscriber row (`throughSeq`) and treats every row up to
// it as let in: the re-founding's reseal (ADR-0090 decision A) seals the new
// epoch seed to each of those keys, and every later rotation does too. A
// fresh admission seed is minted for invitations made from now on. Cost,
// stated: a row the service itself added before the recovery is let in too.
//
// iOS mirror of `apps/web/src/channels/recovered-channels.ts` and
// `apps/web/src/channels/returned-invitations.ts`.

public enum RecoveredChannelRules {
    /// The admission mark a re-founding keeps: the recovery's mark for a
    /// recovered channel, else 0.
    public static func refoundAdmittedThrough(_ throughSeq: UInt64?) -> UInt64 {
        throughSeq ?? 0
    }

    /// The highest subscriber row of a listing (`held` for none).
    public static func highestSeq(_ seqs: [UInt64], held: UInt64 = 0) -> UInt64 {
        seqs.reduce(held) { max($0, $1) }
    }

    public enum Visibility: Equatable, Sendable { case publicChannel, privateChannel }

    /// The type a recovered channel takes from its newest metadata record,
    /// `nil` while none is held.
    public static func visibility(metaListed: Bool?) -> Visibility? {
        guard let metaListed else { return nil }
        return metaListed ? .publicChannel : .privateChannel
    }
}

/// Every recovered channel of an account: channel id (hex) -> `throughSeq`.
public struct RecoveredChannelStore {
    static let prefix = "lakko.channel-recovered.v1."

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func read(accountIdB64: String) -> [String: UInt64] {
        guard let data = defaults.data(forKey: Self.prefix + accountIdB64),
              let raw = try? JSONDecoder().decode([String: UInt64].self, from: data) else { return [:] }
        return raw
    }

    /// Record one (a later recovery of the same channel raises the mark).
    public func mark(_ idHex: String, throughSeq: UInt64, accountIdB64: String) {
        var all = read(accountIdB64: accountIdB64)
        all[idHex] = max(throughSeq, all[idHex] ?? 0)
        write(all, accountIdB64: accountIdB64)
    }

    /// The type is settled: forget it.
    public func clear(_ idHex: String, accountIdB64: String) {
        var all = read(accountIdB64: accountIdB64)
        guard all.removeValue(forKey: idHex) != nil else { return }
        write(all, accountIdB64: accountIdB64)
    }

    private func write(_ all: [String: UInt64], accountIdB64: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        if all.isEmpty {
            defaults.removeObject(forKey: Self.prefix + accountIdB64)
        } else if let data = try? JSONEncoder().encode(all) {
            defaults.set(data, forKey: Self.prefix + accountIdB64)
        }
    }

    public func clearAll(accountIdB64: String) {
        defaults.removeObject(forKey: Self.prefix + accountIdB64)
    }
}

/// ADR-0101 section 7 — a channel return whose old card binding does NOT
/// verify under this account's previous key is shown as the ordinary channel
/// invitation card (Subscribe, or Accept for a publisher invitation), above
/// the chat list, for the person to decide on. Never accepted silently and
/// never dropped; it leaves once the account reads the channel. Kept per
/// account: channel id -> standard base64 of the invitation card (the card
/// carries a link secret, as an invitation message in the log does). The
/// newest card per channel wins.
@MainActor
public final class ReturnedInvitationStore: ObservableObject {
    public static let shared = ReturnedInvitationStore()

    nonisolated static let prefix = "lakko.channel-return-invitations.v1."

    /// Bumped on every change.
    @Published public private(set) var version: UInt64 = 0

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func read(accountIdB64: String) -> [String: String] {
        guard let data = defaults.data(forKey: Self.prefix + accountIdB64),
              let raw = try? JSONDecoder().decode([String: String].self, from: data) else { return [:] }
        return raw.filter { Self.isChannelIdHex($0.key) && !$0.value.isEmpty }
    }

    private static func isChannelIdHex(_ s: String) -> Bool {
        s.count == 32 && s.allSatisfy { ("0"..."9").contains($0) || ("a"..."f").contains($0) }
    }

    /// Keep one (replacing the held card of that channel). Throws when the
    /// account's stores are sealed, which keeps the return held.
    public func keep(channelIdHex: String, cardB64: String, accountIdB64: String) throws {
        var held = read(accountIdB64: accountIdB64)
        held[channelIdHex] = cardB64
        let gate = try AccountStoreGate.enterOrEnd(accountIdB64)
        defer { gate.leave() }
        defaults.set(try JSONEncoder().encode(held), forKey: Self.prefix + accountIdB64)
        version &+= 1
    }

    /// Drop the invitations of channels the account now reads.
    public func drop(channelIdsHex: [String], accountIdB64: String) {
        var held = read(accountIdB64: accountIdB64)
        var changed = false
        for id in channelIdsHex where held.removeValue(forKey: id) != nil { changed = true }
        guard changed, let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        if held.isEmpty {
            defaults.removeObject(forKey: Self.prefix + accountIdB64)
        } else if let data = try? JSONEncoder().encode(held) {
            defaults.set(data, forKey: Self.prefix + accountIdB64)
        }
        version &+= 1
    }

    /// The invitations to show: every held one whose channel the account
    /// does not read yet, in channel id order. Pure.
    public nonisolated static func toShow(
        held: [String: String], reading: Set<String>
    ) -> [(channelIdHex: String, cardB64: String)] {
        held.keys.filter { !reading.contains($0) }.sorted().map { ($0, held[$0]!) }
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.prefix + accountIdB64)
        version &+= 1
    }

    public nonisolated static func clearStored(accountIdB64: String, defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: prefix + accountIdB64)
    }
}
