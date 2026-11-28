// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0101 section 7 — a channel comes back to a person who reset their keys.
//
// A channel return (`composeChannelReturnEnvelope`) travels in a sealed
// mailbox row (kind `channelReturn`) to the reset person's CONTINUITY
// mailbox. It carries the channel id, the role the sender says the person
// had, the OLD member card's account binding, the newest epoch seeds the
// sender holds (at most 64), optionally the person's newest profile, and an
// INVITATION: for a subscriber the channel's invitation card, for a
// publisher the one-use publisher invitation card. An owner return carries
// none.
//
// | this device                        | the reset account was           | sends role |
// |------------------------------------|---------------------------------|------------|
// | owner or publisher of the channel  | a subscriber with a member card | subscriber |
// | an owner device                    | a publisher                     | publisher  |
//
// The reset device verifies the old card binding against its own previous
// account key; only a verified return is acted on without a prompt. An
// unverified one with an invitation is SHOWN as an ordinary invitation card.
//
// iOS mirror of `apps/web/src/channels/channel-return.ts`.

/// The role a return names. There is no owner role since 2026-11-26: an
/// owner takes its channels back from its own list (``OwnedChannels``), with
/// no subscriber or publisher taking part, and the core's decoder refuses
/// the old owner return.
public enum ChannelReturnRoleName: String, Codable, Equatable, Sendable {
    case subscriber
    case publisher

    init(_ ffi: FfiChannelReturnRole) {
        switch ffi {
        case .subscriber: self = .subscriber
        case .publisher: self = .publisher
        }
    }

    var ffi: FfiChannelReturnRole {
        switch self {
        case .subscriber: return .subscriber
        case .publisher: return .publisher
        }
    }
}

/// What the reset device does with one decoded return.
public enum ChannelReturnAction: Equatable, Sendable {
    /// Subscribe with the carried invitation card, then add the seeds.
    case subscribe
    /// Accept the carried publisher invitation, then add the seeds.
    case acceptPublisher
    /// Already in the channel in that role: only the seeds are added.
    case addSeeds
    /// Not verified, but it carries an invitation: shown as an ordinary
    /// invitation for the person to decide on.
    case showInvitation
    /// Nothing to do.
    case drop
}

/// One member card this device opened, as the peer side needs it.
public struct ReturnCard: Equatable, Sendable {
    /// Standard base64 of the account identity key the card opens to.
    public let identityPkB64: String
    /// Standard base64 of the 32-byte subscriber key the card was uploaded
    /// under.
    public let subscriberPkB64: String

    public init(identityPkB64: String, subscriberPkB64: String) {
        self.identityPkB64 = identityPkB64
        self.subscriberPkB64 = subscriberPkB64
    }
}

/// One channel this device is in, as the peer side needs it.
public struct ReturnChannel: Equatable, Sendable {
    public enum OwnRole: Equatable, Sendable { case owner, publisher, subscriber }

    public let idHex: String
    public let ownRole: OwnRole
    public let cards: [ReturnCard]
    /// Standard base64 of the owner's account key, when known.
    public let ownerKeyB64: String?
    /// Standard base64 of the publishers' account keys (owner excluded).
    public let publisherKeysB64: [String]

    public init(idHex: String, ownRole: OwnRole, cards: [ReturnCard], ownerKeyB64: String?, publisherKeysB64: [String]) {
        self.idHex = idHex
        self.ownRole = ownRole
        self.cards = cards
        self.ownerKeyB64 = ownerKeyB64
        self.publisherKeysB64 = publisherKeysB64
    }
}

/// One return to send.
public struct PlannedReturn: Equatable, Sendable {
    public let idHex: String
    public let role: ChannelReturnRoleName
    /// The reset account's account id (standard base64).
    public let accountIdB64: String
    /// The card the return carries the binding of.
    public let card: ReturnCard?
}

public enum ChannelReturnRules {
    /// | verified | role       | invitation | already in role | action          |
    /// |----------|------------|------------|-----------------|-----------------|
    /// | no       | any        | yes        | -               | showInvitation  |
    /// | no       | any        | no         | -               | drop            |
    /// | yes      | subscriber | -          | yes             | addSeeds        |
    /// | yes      | subscriber | yes        | no              | subscribe       |
    /// | yes      | subscriber | no         | no              | drop            |
    /// | yes      | publisher  | -          | yes             | addSeeds        |
    /// | yes      | publisher  | yes        | no              | acceptPublisher |
    /// | yes      | publisher  | no         | no              | addSeeds        |
    public static func action(
        verified: Bool, role: ChannelReturnRoleName, hasInvitation: Bool,
        alreadyInRole: Bool
    ) -> ChannelReturnAction {
        guard verified else { return hasInvitation ? .showInvitation : .drop }
        switch role {
        case .subscriber:
            if alreadyInRole { return .addSeeds }
            return hasInvitation ? .subscribe : .drop
        case .publisher:
            return hasInvitation && !alreadyInRole ? .acceptPublisher : .addSeeds
        }
    }

    /// The returns this device owes, over its channels and the resets it
    /// accepted (`superseded`: every retired key -> its account id). Nothing
    /// is sent for this account's own keys. Pure.
    public static func returnsFor(
        channels: [ReturnChannel], superseded: [String: String], ownKeysB64: Set<String>
    ) -> [PlannedReturn] {
        var out: [PlannedReturn] = []
        for ch in channels {
            // A subscriber's device sends nothing: an owner comes back by
            // itself (``OwnedChannels``), and only a publishing device holds
            // the list of who reads.
            if ch.ownRole == .subscriber { continue }
            let publishers = Set(ch.publisherKeysB64)
            for card in ch.cards {
                if ownKeysB64.contains(card.identityPkB64) || card.identityPkB64 == ch.ownerKeyB64 { continue }
                guard let acct = superseded[card.identityPkB64] else { continue }
                if publishers.contains(card.identityPkB64) {
                    // A publisher's new devices are admitted by an owner device only.
                    if ch.ownRole == .owner {
                        out.append(PlannedReturn(idHex: ch.idHex, role: .publisher, accountIdB64: acct, card: card))
                    }
                    continue
                }
                out.append(PlannedReturn(idHex: ch.idHex, role: .subscriber, accountIdB64: acct, card: card))
            }
        }
        return out
    }

    /// The key a sent return is recorded under.
    public static func sentKey(idHex: String, role: ChannelReturnRoleName, accountIdB64: String) -> String {
        "\(idHex)|\(role.rawValue)|\(accountIdB64)"
    }

    /// The planned returns not yet sent for the accepted generation of their
    /// account (`generationOf`: `nil` when the account has none any more).
    public static func unsent(
        _ planned: [PlannedReturn], sent: [String: UInt64], generationOf: (String) -> UInt64?
    ) -> [PlannedReturn] {
        planned.filter { r in
            guard let gen = generationOf(r.accountIdB64) else { return false }
            guard let was = sent[sentKey(idHex: r.idHex, role: r.role, accountIdB64: r.accountIdB64)] else {
                return true
            }
            return was < gen
        }
    }

    /// The newest seeds a return carries: at most 64.
    public static let maxSeeds = 64

    /// The newest ``maxSeeds`` of the held seeds, ascending, one per epoch,
    /// each seed 32 bytes.
    public static func newestSeeds(_ seeds: [(epoch: UInt32, seed: Data)]) -> [FfiChannelEpochSeed] {
        var unique: [UInt32: Data] = [:]
        for s in seeds where s.seed.count == 32 { unique[s.epoch] = s.seed }
        let epochs = unique.keys.sorted().suffix(maxSeeds)
        return epochs.map { FfiChannelEpochSeed(epoch: $0, seed: unique[$0]!) }
    }

    /// The profile a return carries for the reset account `accountIdB64`:
    /// the newest RAW envelope held under any of THAT account's retired keys
    /// (`superseded`: retired key -> account id), newest stamp wins, never a
    /// key of another account; `nil` when none is held. Pure.
    public static func returnProfile(
        accountIdB64: String, superseded: [String: String],
        heldEnvelopeOf: (String) -> (envelope: Data, updatedAtMs: UInt64)?
    ) -> Data? {
        let keys = superseded.filter { $0.value == accountIdB64 }.map(\.key).sorted()
        return ProfileReturn.newestHeldEnvelope(keysB64: keys, heldEnvelopeOf: heldEnvelopeOf)
    }

    /// The owner's account key of a channel, where this device knows it (a
    /// co-publisher's register carries it): a card of the owner gets neither
    /// a subscriber nor a publisher return. Pure.
    static func ownerKeyB64(extras: ChannelExtras) -> String? {
        guard let key = extras.ownerAccountKey, key.count == 32 else { return nil }
        return base64Encode(key)
    }

    /// The owned channels still owing a recovery-key registration. Pure.
    public static func owingRecoveryKey(owned: [String], registered: Set<String>) -> [String] {
        owned.filter { !registered.contains($0) }.sorted()
    }
}

/// The reset side's held returns, per account: standard base64 of each
/// envelope read from the continuity mailbox and not handled yet, oldest
/// first, bounded.
public struct HeldChannelReturnStore {
    static let prefix = "lakko.channel-returns.v1."
    /// How many returns are held at most (the oldest goes first; chosen).
    public static let maxHeld = 256

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func read(accountIdB64: String) -> [String] {
        defaults.stringArray(forKey: Self.prefix + accountIdB64) ?? []
    }

    /// Hold one more (a duplicate is held once). Throws when the account's
    /// stores are sealed, which holds the mailbox cursor.
    public func hold(_ envelopeB64: String, accountIdB64: String) throws {
        var held = read(accountIdB64: accountIdB64).filter { $0 != envelopeB64 }
        held.append(envelopeB64)
        if held.count > Self.maxHeld { held.removeFirst(held.count - Self.maxHeld) }
        let gate = try AccountStoreGate.enterOrEnd(accountIdB64)
        defer { gate.leave() }
        defaults.set(held, forKey: Self.prefix + accountIdB64)
    }

    /// Drop handled returns.
    public func drop(_ handled: [String], accountIdB64: String) {
        let done = Set(handled)
        let left = read(accountIdB64: accountIdB64).filter { !done.contains($0) }
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        if left.isEmpty {
            defaults.removeObject(forKey: Self.prefix + accountIdB64)
        } else {
            defaults.set(left, forKey: Self.prefix + accountIdB64)
        }
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.prefix + accountIdB64)
    }
}

/// The record of returns sent, per account: `idHex|role|accountId` -> the
/// reset generation it was sent for.
public struct SentChannelReturnStore {
    static let prefix = "lakko.channel-returns-sent.v1."

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func read(accountIdB64: String) -> [String: UInt64] {
        guard let data = defaults.data(forKey: Self.prefix + accountIdB64) else { return [:] }
        return (try? JSONDecoder().decode([String: UInt64].self, from: data)) ?? [:]
    }

    public func write(_ sent: [String: UInt64], accountIdB64: String) {
        guard let data = try? JSONEncoder().encode(sent), let gate = AccountStoreGate.enter(accountIdB64) else {
            return
        }
        defer { gate.leave() }
        defaults.set(data, forKey: Self.prefix + accountIdB64)
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.prefix + accountIdB64)
    }
}

/// The owned channels whose recovery key landed, per account.
public struct ChannelRecoveryRegisteredStore {
    /// `v2`: the recovery key comes from the owned-channels list key since
    /// 2026-11-26, so every owned channel registers again (the keys made
    /// through the removed auth evaluation are not this account's any more).
    static let prefix = "lakko.channel-recovery-registered.v2."

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func read(accountIdB64: String) -> Set<String> {
        Set(defaults.stringArray(forKey: Self.prefix + accountIdB64) ?? [])
    }

    public func mark(_ idHex: String, accountIdB64: String) {
        var held = read(accountIdB64: accountIdB64)
        guard held.insert(idHex).inserted, let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        defaults.set(held.sorted(), forKey: Self.prefix + accountIdB64)
    }

    /// Forget one channel's registration (taken back: registered again
    /// under the new owner key at the next pass).
    public func unmark(_ idHex: String, accountIdB64: String) {
        var held = read(accountIdB64: accountIdB64)
        guard held.remove(idHex) != nil, let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        defaults.set(held.sorted(), forKey: Self.prefix + accountIdB64)
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.prefix + accountIdB64)
    }
}
