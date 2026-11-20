// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

/// ADR-0050 section 2.9 (amended 2026-11-13), `docs/ux/second-device.md`
/// section 4a — a channel reaches every device of the account through the
/// synced channel subscription register (`lakko-sync`), one register per
/// channel id carrying the account's ONE subscriber key, the read key, the
/// role and, for a co-publisher, the joined PublisherGroup id.
///
/// This device writes a register where it starts or ends a channel
/// relationship (subscribe, create, join as co-publisher, unsubscribe,
/// delete, removal), and reconciles its channel list with the registers the
/// fold holds: a live register for a channel this device does not list adds
/// it, with the register's subscriber key (never a fresh one); a left one
/// drops it as an unsubscribe would, without calling the service (the device
/// that left already did). A device that lists a channel with no register
/// writes one (the channels subscribed before this build).
///
/// Local intents are kept owed (``OwedStore``) until the synced state is
/// loaded and they are folded, and are folded BEFORE the registers are read,
/// so a register another device wrote earlier never undoes what this device
/// just did (a subscribe after a held leave, an unsubscribe before the fold
/// is loaded).
@MainActor
final class ChannelSubscriptionSync {
    static let shared = ChannelSubscriptionSync()

    /// What the coordinator needs of the synced state (the DeviceGroup sync
    /// repository in the app, a fake in tests).
    @MainActor
    protocol RegisterPort: AnyObject {
        var registersReady: Bool { get }
        var registers: [ChannelSubscriptionRegister] { get }
        /// Ended conversations (ADR-0087 section 9) that are channels, as
        /// lower-case hex ids.
        var endedChannelIds: Set<String> { get }
        func writeLive(_ write: ChannelSubscriptionPlan.Write) -> Bool?
        func writeLeave(channelId: Data) -> Bool?
    }

    private weak var channels: ChannelsUiState?
    private weak var port: RegisterPort?
    private var accountIdB64: String?
    private var owed: OwedStore?
    private var watch: Set<AnyCancellable> = []
    private var scheduled = false
    /// Mints the subscriber secret of a channel this device publishes and
    /// holds no subscriber key for (a test pins it).
    var mintSecret: () -> Data = { ChannelsComposer.freshSubscriberKeys().secret }
    /// Leaves the service row of a subscriber key this device replaced by
    /// the register's (best effort; a test records it).
    var unsubscribeOldKey: (ChannelSubscriberKeys, String) -> Void = { keys, channelId in
        Task { await ChannelUnsubscribe.run(keys: keys, channelId: channelId) }
    }

    /// Attach to one account's channel state and synced state. Wires the
    /// state's hooks and reconciles whenever either side changes.
    func bind(
        accountIdB64: String, channels: ChannelsUiState, port: RegisterPort,
        changes: AnyPublisher<Void, Never>, defaults: UserDefaults = .standard
    ) {
        unbind()
        self.accountIdB64 = accountIdB64
        self.channels = channels
        self.port = port
        owed = OwedStore(accountIdB64: accountIdB64, defaults: defaults)
        channels.registeredSubscriberKeys = { [weak self] id in self?.registeredKeys(channelId: id) }
        channels.onRelationshipStarted = { [weak self] id in self?.noteStarted(channelId: id) }
        channels.onRelationshipEnded = { [weak self] id in self?.noteEnded(channelId: id) }
        channels.onPublishingEnded = { [weak self] id in self?.notePublishingEnded(channelId: id) }
        Publishers.MergeMany(
            changes,
            channels.$channels.map { _ in () }.eraseToAnyPublisher(),
            channels.$subscriberKeysByChannel.map { _ in () }.eraseToAnyPublisher(),
            channels.$keysByChannel.map { _ in () }.eraseToAnyPublisher())
            .sink { [weak self] in self?.scheduleReconcile() }
            .store(in: &watch)
        scheduleReconcile()
    }

    func unbind() {
        watch = []
        channels?.registeredSubscriberKeys = nil
        channels?.onRelationshipStarted = nil
        channels?.onRelationshipEnded = nil
        channels?.onPublishingEnded = nil
        channels = nil
        port = nil
        accountIdB64 = nil
        owed = nil
        scheduled = false
    }

    /// The account the coordinator is bound to.
    var boundAccountIdB64: String? { accountIdB64 }

    // MARK: - Local intents

    private func noteStarted(channelId: String) {
        owed?.set(channelId, .live)
        scheduleReconcile()
    }

    private func noteEnded(channelId: String) {
        owed?.set(channelId, .left)
        // Folded now when the synced state is loaded: the channel just left
        // the list, and a reconcile reading the still-live register would put
        // it back.
        flushOwed()
    }

    private func notePublishingEnded(channelId: String) {
        owed?.set(channelId, .liveSubscriber)
        scheduleReconcile()
    }

    /// The subscriber key of the channel's live register, or `nil`.
    func registeredKeys(channelId: String) -> ChannelSubscriberKeys? {
        guard let port, port.registersReady,
              let register = port.registers.first(where: { hexEncode($0.channelId) == channelId.lowercased() }),
              register.subscribed,
              let kp = try? ChannelSubscriberKeypair.fromSecret(secret: register.subscriberSecret)
        else { return nil }
        return ChannelSubscriberKeys(secret: register.subscriberSecret, publicKey: kp.`public`())
    }

    // MARK: - Reconcile

    private func scheduleReconcile() {
        guard !scheduled else { return }
        scheduled = true
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 300_000_000)
            guard let self else { return }
            self.scheduled = false
            self.reconcileNow()
        }
    }

    /// Fold the owed intents, then bring the channel list and the registers
    /// in line. Idempotent; a no-op until the synced state is loaded.
    func reconcileNow() {
        guard let channels, let port, port.registersReady else { return }
        flushOwed()
        let plan = ChannelSubscriptionPlan.plan(
            registers: port.registers,
            locals: locals(channels),
            ended: port.endedChannelIds,
            owedLeaves: owed?.leaves ?? [],
            leftAlone: channels.channelsLeftAloneByRegister(),
            mintSecret: mintSecret)
        apply(plan.applies, channels: channels)
        for write in plan.writes {
            _ = port.writeLive(write)
        }
    }

    /// Write every owed intent the fold can take now.
    private func flushOwed() {
        guard let channels, let port, port.registersReady, let owed else { return }
        let registers = port.registers
        let leftAlone = channels.channelsLeftAloneByRegister()
        for (id, kind) in owed.all {
            guard let channelId = CoPublisher.hexToData(id), channelId.count == 16, !leftAlone.contains(id) else {
                owed.remove(id)
                continue
            }
            switch kind {
            case .left:
                if port.writeLeave(channelId: channelId) != nil { owed.remove(id) }
            case .live, .liveSubscriber:
                let register = registers.first { $0.channelId == channelId }
                guard let local = locals(channels).first(where: { $0.id == id }), local.listed,
                      let write = ChannelSubscriptionPlan.desired(
                        local: local, register: register, forceSubscriber: kind == .liveSubscriber,
                        mintSecret: mintSecret)
                else {
                    owed.remove(id)
                    continue
                }
                if port.writeLive(write) != nil { owed.remove(id) }
            }
        }
    }

    private func locals(_ channels: ChannelsUiState) -> [ChannelSubscriptionPlan.Local] {
        let listed = Set(channels.channels.map { $0.id.lowercased() })
        var out: [ChannelSubscriptionPlan.Local] = channels.channels.map { c in
            let id = c.id.lowercased()
            let extras = channels.extras(c.id)
            return ChannelSubscriptionPlan.Local(
                id: id,
                listed: true,
                subscriberSecret: channels.subscriberKeysByChannel[c.id]?.secret,
                publishesHere: channels.keysByChannel[c.id] != nil,
                coPublishedGroupIdHex: CoPublishedJournals.shared.journalIdHex(channelIdHex: c.id),
                readKey: extras.readKey,
                accountRole: extras.accountRole,
                ownerAccountKey: extras.ownerAccountKey,
                name: c.sharedName.isEmpty ? c.localName : c.sharedName,
                handle: c.handle,
                holdsSeeds: !channels.subscribedSeeds(channelId: c.id).isEmpty)
        }
        // Seeds a pairing bundle carried for a channel whose register has
        // not arrived (or has left).
        for id in channels.subscribedSeedsByChannel.keys where !listed.contains(id.lowercased()) {
            out.append(ChannelSubscriptionPlan.Local(
                id: id.lowercased(), listed: false, subscriberSecret: nil, publishesHere: false,
                coPublishedGroupIdHex: nil, readKey: nil, accountRole: nil, ownerAccountKey: nil,
                name: "", handle: "",
                holdsSeeds: true))
        }
        return out
    }

    private func apply(_ applies: [ChannelSubscriptionPlan.Apply], channels: ChannelsUiState) {
        for step in applies {
            switch step {
            case .add(let id, let name, let handle):
                // The list row first; the keys and extras follow below.
                channels.addChannel(ChannelHandle(
                    id: id, localName: name, localDescription: "",
                    addedAtMs: UInt64(Date().timeIntervalSince1970 * 1000), handle: handle))
            case .setKey(let id, let secret, let replacing):
                guard let kp = try? ChannelSubscriberKeypair.fromSecret(secret: secret) else { continue }
                if let replacing, let old = channels.subscriberKeysByChannel[id], old.secret == replacing {
                    unsubscribeOldKey(old, id)
                }
                channels.setSubscriberKeys(
                    channelId: id, keys: ChannelSubscriberKeys(secret: secret, publicKey: kp.`public`()))
            case .extras(let id, let role, let readKey, let coPublishedGroupId, let ownerAccountKey):
                channels.updateExtras(id) {
                    $0.accountRole = role
                    if let readKey { $0.readKey = readKey }
                    if let ownerAccountKey { $0.ownerAccountKey = ownerAccountKey }
                }
                if let coPublishedGroupId,
                   CoPublishedJournals.shared.journalIdHex(channelIdHex: id) == nil {
                    // Never claims this journal, never founds it: the group
                    // id is the owner's (ADR-0078 section 6).
                    try? CoPublishedJournals.shared.markOrEnd(
                        journalIdHex: hexEncode(coPublishedGroupId), channelIdHex: id,
                        accountIdB64: accountIdB64)
                }
            case .drop(let id):
                // As this device's own unsubscribe, without the service call
                // and without telling the other devices (they told this one).
                ChannelMemberCards.forgetUploads(channelId: id)
                channels.clearSubscriberKeys(id)
                channels.removeChannel(id)
            case .dropCarried(let id):
                channels.clearSubscriberKeys(id)
            }
        }
    }

    // MARK: - Pairing bundle seeds (ADR-0050 section 2.9, bundle v4)

    /// The seeds this device hands a new device: one entry per channel with
    /// a live register for which it holds seeds, ascending by channel id
    /// bytes, each the newest ``StateBundle/maxSeedsPerChannel`` seeds
    /// ascending by epoch.
    func carriedSeedsForBundle() -> [StateBundle.CarriedChannelSeeds] {
        guard let channels, let port, port.registersReady else { return [] }
        let live = port.registers.filter(\.subscribed).map { hexEncode($0.channelId) }
        var held: [String: [UInt32: Data]] = [:]
        for id in live {
            guard let listed = channels.channels.first(where: { $0.id.lowercased() == id }) else { continue }
            let seeds = channels.readSeeds(channelId: listed.id)
            if !seeds.isEmpty { held[id] = seeds }
        }
        return StateBundle.carriedChannelSeeds(held, perChannel: StateBundle.maxSeedsPerChannel)
    }

    /// Keep the seeds a bundle carried: added to each channel's epoch map
    /// unless one is held for that epoch. Through the live state when it is
    /// bound to `accountIdB64`, else into the stored channel log.
    func storeCarriedSeeds(accountIdB64: String, carried: [StateBundle.CarriedChannelSeeds]) {
        let decoded = StateBundle.decodedChannelSeeds(carried)
        guard !decoded.isEmpty else { return }
        if let channels, self.accountIdB64 == accountIdB64 {
            for (id, seeds) in decoded {
                let held = channels.subscribedSeeds(channelId: id)
                for (epoch, seed) in seeds where held[epoch] == nil {
                    channels.setSubscribedSeed(channelId: id, epoch: epoch, seed: seed)
                }
            }
            scheduleReconcile()
            return
        }
        let store = ChannelLogStore()
        let payload = store.get(accountIdB64: accountIdB64) ?? .empty
        let merged = StateBundle.mergedChannelSeeds(held: payload.subscribedSeedsByChannel, carried: decoded)
        store.put(accountIdB64: accountIdB64, payload: ChannelLogPayload(
            channels: payload.channelHandles, subscribedSeedsByChannel: merged))
    }

    // MARK: - Owed intents

    /// One owed intent per channel, per account, in `UserDefaults` behind the
    /// account store gate. Not secret: channel ids and a word.
    final class OwedStore {
        enum Kind: String, Codable { case live, liveSubscriber, left }

        private let key: String
        private let accountIdB64: String
        private let defaults: UserDefaults

        init(accountIdB64: String, defaults: UserDefaults = .standard) {
            self.accountIdB64 = accountIdB64
            self.defaults = defaults
            key = Self.key(accountIdB64)
        }

        static func key(_ accountIdB64: String) -> String {
            "lakko.channel-subscriptions.owed.v1.\(accountIdB64)"
        }

        var all: [(String, Kind)] {
            let map = (defaults.dictionary(forKey: key) as? [String: String]) ?? [:]
            return map.compactMap { id, raw in Kind(rawValue: raw).map { (id, $0) } }.sorted { $0.0 < $1.0 }
        }

        var leaves: Set<String> { Set(all.filter { $0.1 == .left }.map(\.0)) }

        func set(_ channelId: String, _ kind: Kind) {
            guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
            defer { gate.leave() }
            var map = (defaults.dictionary(forKey: key) as? [String: String]) ?? [:]
            map[channelId.lowercased()] = kind.rawValue
            defaults.set(map, forKey: key)
        }

        func remove(_ channelId: String) {
            guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
            defer { gate.leave() }
            var map = (defaults.dictionary(forKey: key) as? [String: String]) ?? [:]
            guard map.removeValue(forKey: channelId.lowercased()) != nil else { return }
            defaults.set(map, forKey: key)
        }

        static func clear(accountIdB64: String, defaults: UserDefaults = .standard) {
            defaults.removeObject(forKey: key(accountIdB64))
        }
    }
}

/// The pure decision ``ChannelSubscriptionSync`` takes: which registers
/// change this device's channel list, and which registers this device
/// writes. Free of the state objects so it is testable without them.
enum ChannelSubscriptionPlan {

    /// One channel as this device holds it.
    struct Local: Equatable {
        /// Lower-case hex channel id.
        let id: String
        /// In the channel list (else: only carried seeds are held).
        let listed: Bool
        let subscriberSecret: Data?
        /// This device holds publisher keys for it (its PublisherGroup).
        let publishesHere: Bool
        /// The joined PublisherGroup id of a co-published channel.
        let coPublishedGroupIdHex: String?
        let readKey: Data?
        /// ``ChannelExtras/accountRole`` as stored.
        let accountRole: String?
        /// ``ChannelExtras/ownerAccountKey``: a co-published channel's owner
        /// key this device recorded (contract 3a).
        var ownerAccountKey: Data? = nil
        let name: String
        let handle: String
        let holdsSeeds: Bool
    }

    /// One change to this device's channel state.
    enum Apply: Equatable {
        /// List the channel (a live register this device did not list).
        case add(id: String, name: String, handle: String)
        /// Install the account's subscriber key; `replacing` names the
        /// secret this device held before, whose service row is left.
        case setKey(id: String, secret: Data, replacing: Data?)
        /// The account's role (``ChannelExtras/accountRole``), a read key
        /// this device lacked, and a co-publisher's group id.
        case extras(
            id: String, role: String?, readKey: Data?, coPublishedGroupId: Data?, ownerAccountKey: Data? = nil)
        /// Unlist a channel the account left, as an unsubscribe does,
        /// without the service call.
        case drop(id: String)
        /// Forget seeds carried for a channel the account left.
        case dropCarried(id: String)
    }

    /// One live register to write.
    struct Write: Equatable {
        let channelId: Data
        let role: ChannelSubscriptionRole
        let subscriberSecret: Data
        let readKey: Data
        let publisherGroupId: Data
        /// ADR-0090 contract 3a — 32 bytes for a co-publisher, else empty.
        var ownerAccountKey: Data = Data()
        let name: String
        let handle: String
    }

    struct Outcome: Equatable {
        var applies: [Apply] = []
        var writes: [Write] = []
    }

    static func plan(
        registers: [ChannelSubscriptionRegister],
        locals: [Local],
        ended: Set<String>,
        owedLeaves: Set<String>,
        leftAlone: Set<String> = [],
        mintSecret: () -> Data
    ) -> Outcome {
        var out = Outcome()
        var byId: [String: Local] = [:]
        for local in locals { byId[local.id] = local }
        var registerById: [String: ChannelSubscriptionRegister] = [:]
        for register in registers { registerById[hexEncode(register.channelId)] = register }

        for register in registers {
            let id = hexEncode(register.channelId)
            // An orphan of a key reset, or one the person removed after it:
            // never listed back, never written (follow-up decision 2).
            if leftAlone.contains(id) { continue }
            let local = byId[id]
            guard register.subscribed else {
                if let local, local.listed, !local.publishesHere {
                    out.applies.append(.drop(id: id))
                } else if let local, !local.listed, local.holdsSeeds {
                    out.applies.append(.dropCarried(id: id))
                }
                continue
            }
            if owedLeaves.contains(id) { continue }
            let role = ChannelAccountRole.stored(register.role)
            let groupId = register.role == .coPublisher && register.publisherGroupId.count == 32
                ? register.publisherGroupId : nil
            let readKey = register.readKey.count == 32 ? register.readKey : nil
            let ownerKey = register.role == .coPublisher && register.ownerAccountKey.count == 32
                ? register.ownerAccountKey : nil
            guard let local, local.listed else {
                // A channel the account ended and has not joined again: the
                // register may be a step behind the end (ADR-0087 section 9).
                if ended.contains(id) { continue }
                out.applies.append(.add(id: id, name: register.name, handle: register.handle))
                out.applies.append(.setKey(id: id, secret: register.subscriberSecret, replacing: nil))
                out.applies.append(.extras(
                    id: id, role: role, readKey: readKey, coPublishedGroupId: groupId, ownerAccountKey: ownerKey))
                continue
            }
            if local.subscriberSecret != register.subscriberSecret,
               local.subscriberSecret != nil || !local.publishesHere {
                // One key per account and channel: the register's wins.
                out.applies.append(.setKey(
                    id: id, secret: register.subscriberSecret, replacing: local.subscriberSecret))
            }
            let wantedRole = local.publishesHere ? localRole(local) : role
            let missingReadKey = local.readKey == nil ? readKey : nil
            let missingMark = local.coPublishedGroupIdHex == nil ? groupId : nil
            let missingOwner = local.ownerAccountKey == nil ? ownerKey : nil
            if wantedRole != local.accountRole || missingReadKey != nil || missingMark != nil
                || missingOwner != nil {
                out.applies.append(.extras(
                    id: id, role: wantedRole, readKey: missingReadKey, coPublishedGroupId: missingMark,
                    ownerAccountKey: missingOwner))
            }
        }

        for local in locals where local.listed && !owedLeaves.contains(local.id) && !leftAlone.contains(local.id) {
            let register = registerById[local.id]
            // A left register drops a reader's row above; a channel this
            // device publishes is the account's still, so it is written live.
            if let register, !register.subscribed, !local.publishesHere { continue }
            guard let write = desired(
                local: local, register: register, forceSubscriber: false, mintSecret: mintSecret)
            else { continue }
            if let register, register.subscribed, same(write, register) { continue }
            out.writes.append(write)
        }
        return out
    }

    /// The register this device's view of `local` stands for. `nil` when it
    /// has nothing to write (no key and not a publisher here, a co-publisher
    /// with no group id, a bad id).
    static func desired(
        local: Local, register: ChannelSubscriptionRegister?, forceSubscriber: Bool,
        mintSecret: () -> Data
    ) -> Write? {
        guard let channelId = CoPublisher.hexToData(local.id), channelId.count == 16 else { return nil }
        let live = register?.subscribed == true ? register : nil
        // The register's key is the account's; this device's own key goes
        // only where none is held (a reconcile adopts the register's first).
        let secret: Data
        if let live, live.subscriberSecret.count == 32 {
            secret = live.subscriberSecret
        } else if let own = local.subscriberSecret, own.count == 32 {
            secret = own
        } else if local.publishesHere {
            secret = mintSecret()
        } else {
            return nil
        }
        let role: ChannelSubscriptionRole
        if forceSubscriber {
            role = .subscriber
        } else if local.publishesHere {
            role = local.coPublishedGroupIdHex != nil ? .coPublisher : .owner
        } else {
            role = live?.role ?? .subscriber
        }
        var groupId = Data()
        var ownerKey = Data()
        if role == .coPublisher {
            groupId = local.coPublishedGroupIdHex.flatMap(CoPublisher.hexToData) ?? live?.publisherGroupId ?? Data()
            guard groupId.count == 32 else { return nil }
            // Contract 3a — required for a co-publisher: the issuer key this
            // account verified at join (here, or as another device wrote it).
            ownerKey = local.ownerAccountKey.flatMap { $0.count == 32 ? $0 : nil }
                ?? live.flatMap { $0.ownerAccountKey.count == 32 ? $0.ownerAccountKey : nil } ?? Data()
            guard ownerKey.count == 32 else { return nil }
        }
        let readKey = local.readKey.flatMap { $0.count == 32 ? $0 : nil }
            ?? live.flatMap { $0.readKey.count == 32 ? $0.readKey : nil } ?? Data()
        let name = live.flatMap { $0.name.isEmpty ? nil : $0.name } ?? sanitizedName(local.name)
        let handle = live.flatMap { $0.handle.isEmpty ? nil : $0.handle } ?? sanitizedHandle(local.handle)
        return Write(
            channelId: channelId, role: role, subscriberSecret: secret, readKey: readKey,
            publisherGroupId: groupId, ownerAccountKey: ownerKey, name: name, handle: handle)
    }

    static func localRole(_ local: Local) -> String? {
        local.coPublishedGroupIdHex != nil ? ChannelAccountRole.coPublisher : ChannelAccountRole.owner
    }

    static func same(_ write: Write, _ register: ChannelSubscriptionRegister) -> Bool {
        register.subscribed && register.role == write.role
            && register.subscriberSecret == write.subscriberSecret && register.readKey == write.readKey
            && register.publisherGroupId == write.publisherGroupId
            && register.ownerAccountKey == write.ownerAccountKey && register.name == write.name
            && register.handle == write.handle
    }

    /// A name the register takes: trimmed, at most 255 bytes, no line break,
    /// control or format character; anything else is left out (empty).
    static func sanitizedName(_ raw: String) -> String {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.utf8.count <= 255 else { return "" }
        for scalar in trimmed.unicodeScalars {
            let category = scalar.properties.generalCategory
            if category == .control || category == .format || category == .lineSeparator
                || category == .paragraphSeparator {
                return ""
            }
        }
        return trimmed
    }

    /// A handle the register takes: without `@`, canonical `[a-z0-9_]`,
    /// 3 to 32 characters; anything else is left out (empty).
    static func sanitizedHandle(_ raw: String) -> String {
        let bare = String(raw.trimmingCharacters(in: .whitespaces).drop(while: { $0 == "@" }))
        guard (3...32).contains(bare.count),
              bare.unicodeScalars.allSatisfy({ ("a"..."z").contains($0) || ("0"..."9").contains($0) || $0 == "_" })
        else { return "" }
        return bare
    }
}

/// The sync repository's side of ``ChannelSubscriptionSync/RegisterPort``.
extension DeviceGroupSyncRepository: ChannelSubscriptionSync.RegisterPort {
    var registersReady: Bool { ready && !deviceRemoved }
    var registers: [ChannelSubscriptionRegister] { channelSubscriptions }
    var endedChannelIds: Set<String> {
        Set(endedConversations.compactMap { register in
            guard case .channel = register.conversation.kind else { return nil }
            return hexEncode(register.conversation.id)
        })
    }

    func writeLive(_ write: ChannelSubscriptionPlan.Write) -> Bool? {
        setChannelSubscription(
            channelId: write.channelId, role: write.role, subscriberSecret: write.subscriberSecret,
            readKey: write.readKey, publisherGroupId: write.publisherGroupId,
            ownerAccountKey: write.ownerAccountKey, name: write.name,
            handle: write.handle)
    }

    func writeLeave(channelId: Data) -> Bool? {
        leaveChannelSubscription(channelId: channelId)
    }
}
