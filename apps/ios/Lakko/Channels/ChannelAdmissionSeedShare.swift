// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0087 section 7 — the owner hands each publisher every admission-seed
/// version it lacks, sealed to that person's ACCOUNT X25519 key, as an
/// application message on the PublisherGroup journal, so any publisher's
/// device can check tickets and let people in. The key comes from the
/// person's identity bundle in the directory, fetched by the account id their
/// request carried in its profile, and is used only when the bundle's
/// signature key IS the account key the request's KeyPackage is bound to.
/// Versions sent are recorded per (channel, publisher); a failure is retried
/// by the next pass. The web and Android clients do the same.
///
/// A co-publisher never mints a seed (it would collide with the owner's
/// version 0 and orphan the tickets sealed to it): it keeps the versions it
/// is sent (``ChannelInvites/putAdmissionSeed(channelIdHex:version:seed:)``).
///
/// The same carrier brings a public channel's READ KEY to each publisher
/// (ADR-0087 phase B), so a co-publisher can write the public seed envelope;
/// a co-publisher never derives one of its own (it would be its lineage's,
/// not the channel's).
///
/// And the channel's EARLIER epoch seeds (ADR-0087 section 7, amended
/// 2026-11-08; UX 15.2.1): a new publisher joins the PublisherGroup at a
/// later MLS epoch and could open no earlier post, not even the owner's
/// epoch-0 record. The owner seals the newest ``maxEpochSeeds`` of its epoch
/// map that it has not sent that publisher yet, one envelope per pass; a
/// receiver adds a seed only for an epoch it holds none of.
@MainActor
enum ChannelAdmissionSeedShare {

    private struct Request: Codable, Equatable {
        let accountKeyHex: String
        let accountIdHex: String
    }

    private struct Record: Codable, Equatable {
        /// Requests retained by the owner, by joiner key.
        var requests: [String: Request] = [:]
        /// Approved publishers, by joiner key.
        var publishers: [String: Request] = [:]
        /// Versions already sealed, by joiner key.
        var sent: [String: [UInt16]] = [:]
        /// Publishers the read key was sealed to, by joiner key.
        var readKeySent: Set<String> = []
        /// Epochs whose seed was sealed to each publisher, by joiner key.
        /// Optional so a record written before it decodes.
        var epochSeedsSent: [String: [UInt32]]?
    }

    private static func key(_ account: String, _ channelId: String) -> String {
        "me.lakko.channel-seed-share.v1.\(account).\(channelId.lowercased())"
    }

    private static func load(_ account: String, _ channelId: String) -> Record {
        guard let data = UserDefaults.standard.data(forKey: key(account, channelId)),
              let record = try? JSONDecoder().decode(Record.self, from: data) else { return Record() }
        return record
    }

    private static func save(_ record: Record, _ account: String, _ channelId: String) {
        guard let gate = AccountStoreGate.enter(account), let data = try? JSONEncoder().encode(record) else { return }
        defer { gate.leave() }
        UserDefaults.standard.set(data, forKey: key(account, channelId))
    }

    private static func hex(_ data: Data) -> String { data.map { String(format: "%02x", $0) }.joined() }

    /// A request the owner's poll retained, with the account id its profile
    /// carried. Kept by the joiner key it came with. A request carries the id
    /// only in a profile, and a person with no display name has none
    /// (ADR-0048): such a request is kept too, with the id this account's
    /// contact row names, or `""` until ``fillPublisherAccountIds(channelId:)``
    /// finds one.
    static func rememberRequests(_ requests: [CoPublisher.JoinRequest], channelId: String) {
        guard let account = AcceptedContactsStore.boundAccountIdB64, !requests.isEmpty else { return }
        var record = load(account, channelId)
        let contacts = AcceptedContactsStore.load()
        for request in requests {
            let carried = request.accountId.flatMap { $0.count == 16 ? hex($0) : nil } ?? ""
            let joinerHex = hex(request.joinerKey)
            let resolved = Self.resolveAccountIdHex(
                recordedHex: carried, accountKey: request.accountKey, contacts: contacts)
            // A later sighting of the same request never forgets an id held.
            if resolved.isEmpty, let held = record.requests[joinerHex], !held.accountIdHex.isEmpty { continue }
            record.requests[joinerHex] = Request(
                accountKeyHex: hex(request.accountKey), accountIdHex: resolved)
        }
        save(record, account, channelId)
    }

    /// ADR-0087 section 7 — the 16-byte account id (lower-case hex) the
    /// owner fetches a publisher's identity bundle by: the one the request
    /// carried, else the one this account's contact row for `accountKey`
    /// names (the person added through "Add publisher" is a contact). The id
    /// only picks the bundle; nothing is sealed unless the bundle proves the
    /// same person (``provesSamePerson(requestKeyHex:bundle:)``). `""` when
    /// neither names one.
    nonisolated static func resolveAccountIdHex(
        recordedHex: String, accountKey: Data, contacts: [AcceptedContact]
    ) -> String {
        func lowerHex(_ d: Data) -> String { d.map { String(format: "%02x", $0) }.joined() }
        if recordedHex.count == 32, let carried = Data(channelHex: recordedHex), carried.count == 16 {
            return recordedHex.lowercased()
        }
        let keyB64 = base64Encode(accountKey)
        for contact in contacts
        where contact.request.fromPubkey == accountKey || (contact.previousKeysB64 ?? []).contains(keyB64) {
            if let id = contact.request.fromAccountId, id.count == 16 { return lowerHex(id) }
        }
        return ""
    }

    /// Fill in the account id of every publisher recorded without one, from
    /// this account's contact store, and keep it. Returns the publishers the
    /// owner can reach now: joiner key (hex) to account id (hex).
    @discardableResult
    static func fillPublisherAccountIds(channelId: String) -> [String: String] {
        guard let account = AcceptedContactsStore.boundAccountIdB64 else { return [:] }
        var record = load(account, channelId)
        var contacts: [AcceptedContact]?
        var changed = false
        for (joinerHex, publisher) in record.publishers where publisher.accountIdHex.count != 32 {
            guard let key = Data(channelHex: publisher.accountKeyHex) else { continue }
            if contacts == nil { contacts = AcceptedContactsStore.load() }
            let resolved = Self.resolveAccountIdHex(
                recordedHex: publisher.accountIdHex, accountKey: key, contacts: contacts ?? [])
            guard !resolved.isEmpty else { continue }
            record.publishers[joinerHex] = Request(accountKeyHex: publisher.accountKeyHex, accountIdHex: resolved)
            changed = true
        }
        if changed { save(record, account, channelId) }
        return record.publishers.compactMapValues { $0.accountIdHex.count == 32 ? $0.accountIdHex : nil }
    }

    /// After an approve: the joiner is a publisher to send seeds to. A
    /// publisher removed and approved again gets a fresh hand-over.
    static func notePublisher(channelId: String, joinerKey: Data) {
        guard let account = AcceptedContactsStore.boundAccountIdB64 else { return }
        var record = load(account, channelId)
        let joinerHex = hex(joinerKey)
        guard let request = record.requests[joinerHex] else { return }
        record.publishers[joinerHex] = request
        record.sent[joinerHex] = nil
        record.readKeySent.remove(joinerHex)
        record.epochSeedsSent?[joinerHex] = nil
        save(record, account, channelId)
    }

    /// After a removal: nothing more is sealed to that publisher.
    static func forgetPublisher(channelId: String, accountKey: Data) {
        guard let account = AcceptedContactsStore.boundAccountIdB64 else { return }
        var record = load(account, channelId)
        let keyHex = hex(accountKey)
        let gone = record.publishers.filter { $0.key == keyHex || $0.value.accountKeyHex == keyHex }.map(\.key)
        guard !gone.isEmpty else { return }
        for joinerHex in gone {
            record.publishers[joinerHex] = nil
            record.sent[joinerHex] = nil
            record.readKeySent.remove(joinerHex)
            record.epochSeedsSent?[joinerHex] = nil
        }
        save(record, account, channelId)
    }

    /// Most seeds in one hand-over (the core's bound).
    static var maxEpochSeeds: Int { Int(channelEpochSeedsMax()) }

    /// The seeds to send a publisher: the newest `max` of the owner's epoch
    /// map that it was not sent yet, ascending by epoch.
    nonisolated static func epochSeedsToSend(
        held: [UInt32: Data], sent: Set<UInt32>, max: Int
    ) -> [(epoch: UInt32, seed: Data)] {
        held.filter { !sent.contains($0.key) && $0.value.count == 32 }
            .sorted { $0.key > $1.key }
            .prefix(max)
            .sorted { $0.key < $1.key }
            .map { (epoch: $0.key, seed: $0.value) }
    }

    /// The received seeds a publisher adds: only for an epoch it holds no
    /// seed of; never one it holds replaced.
    nonisolated static func epochSeedsToKeep(
        received: [(epoch: UInt32, seed: Data)], held: [UInt32: Data]
    ) -> [(epoch: UInt32, seed: Data)] {
        var seen = Set(held.keys)
        var out: [(epoch: UInt32, seed: Data)] = []
        for item in received where !seen.contains(item.epoch) && item.seed.count == 32 {
            seen.insert(item.epoch)
            out.append(item)
        }
        return out
    }

    /// The owner's pass: seal what each publisher lacks. Best effort.
    static func sendOwed(
        channelId: String, identity: Identity, group: PublisherGroup, ds: DsClient, readKey: Data? = nil,
        epochSeeds: [UInt32: Data] = [:]
    ) async {
        guard let account = AcceptedContactsStore.boundAccountIdB64 else { return }
        let seeds = ChannelInvites.admissionSeeds(channelIdHex: channelId)
        guard !seeds.isEmpty || readKey != nil || !epochSeeds.isEmpty else { return }
        let reachable = fillPublisherAccountIds(channelId: channelId)
        var record = load(account, channelId)
        guard !record.publishers.isEmpty, let channelBytes = try? group.channelId(),
              let groupId = try? group.groupId() else { return }
        for (joinerHex, publisher) in record.publishers {
            let already = Set(record.sent[joinerHex] ?? [])
            let owed = seeds.filter { !already.contains($0.version) }
            let readKeyOwed = readKey != nil && !record.readKeySent.contains(joinerHex)
            let epochSeedsOwed = Self.epochSeedsToSend(
                held: epochSeeds, sent: Set(record.epochSeedsSent?[joinerHex] ?? []), max: maxEpochSeeds)
            guard !owed.isEmpty || readKeyOwed || !epochSeedsOwed.isEmpty,
                  let accountIdHex = reachable[joinerHex],
                  let accountId = Data(channelHex: accountIdHex), accountId.count == 16 else { continue }
            let discovery = DiscoveryClient(sessionToken: AmbientSessionToken.shared.current)
            guard let bundle = try? await discovery.identityFetch(accountId: accountId),
                  let accountKey = Data(base64Encoded: bundle.signaturePubkey),
                  Self.provesSamePerson(requestKeyHex: publisher.accountKeyHex, bundle: bundle),
                  let x25519 = Data(base64Encoded: bundle.x25519Pubkey), x25519.count == 32
            else { continue }
            for seed in owed {
                do {
                    let envelope = try composeAdmissionSeedEnvelope(
                        channelId: channelBytes, memberAccountKey: accountKey,
                        memberX25519PublicKey: x25519, admissionVersion: seed.version,
                        admissionSeed: seed.seed)
                    let message = try group.createApplicationMessage(identity: identity, plaintext: envelope)
                    // The send ratchet moved: on disk before the message leaves, so a
                    // crash never reuses a generation (the G-34 rule).
                    try ChannelPublisherIdentity.shared.persistOrThrow()
                    _ = try await ds.publishGroup(
                        groupId: groupId, payload: message,
                        auth: CoPublisher.auth(ds: ds, identity: identity, publisherGroup: group))
                    record.sent[joinerHex, default: []].append(seed.version)
                    save(record, account, channelId)
                } catch {
                    NSLog("[channels] an admission seed was not sealed to a publisher yet")
                    break
                }
            }
            if readKeyOwed, let readKey {
                do {
                    let envelope = try composeChannelReadKeyEnvelope(
                        channelId: channelBytes, memberAccountKey: accountKey,
                        memberX25519PublicKey: x25519, readKey: readKey)
                    let message = try group.createApplicationMessage(identity: identity, plaintext: envelope)
                    // The send ratchet moved: on disk before the message leaves, so a
                    // crash never reuses a generation (the G-34 rule).
                    try ChannelPublisherIdentity.shared.persistOrThrow()
                    _ = try await ds.publishGroup(
                        groupId: groupId, payload: message,
                        auth: CoPublisher.auth(ds: ds, identity: identity, publisherGroup: group))
                    record.readKeySent.insert(joinerHex)
                    save(record, account, channelId)
                } catch {
                    NSLog("[channels] a read key was not sealed to a publisher yet")
                }
            }
            if !epochSeedsOwed.isEmpty {
                do {
                    let envelope = try composeChannelEpochSeedsEnvelope(
                        channelId: channelBytes, memberAccountKey: accountKey,
                        memberX25519PublicKey: x25519,
                        seeds: epochSeedsOwed.map { FfiChannelEpochSeed(epoch: $0.epoch, seed: $0.seed) })
                    let message = try group.createApplicationMessage(identity: identity, plaintext: envelope)
                    // The send ratchet moved: on disk before the message leaves, so a
                    // crash never reuses a generation (the G-34 rule).
                    try ChannelPublisherIdentity.shared.persistOrThrow()
                    _ = try await ds.publishGroup(
                        groupId: groupId, payload: message,
                        auth: CoPublisher.auth(ds: ds, identity: identity, publisherGroup: group))
                    var sentEpochs = record.epochSeedsSent ?? [:]
                    sentEpochs[joinerHex, default: []].append(contentsOf: epochSeedsOwed.map(\.epoch))
                    record.epochSeedsSent = sentEpochs
                    save(record, account, channelId)
                } catch {
                    NSLog("[channels] earlier seeds were not sealed to a publisher yet")
                }
            }
        }
    }

    /// Whether the bundle's current key is the person the request's account
    /// key belongs to (ADR-0074 section 3.7):
    /// - the same key;
    /// - the bundle's two-way conversation link names the request's key: it
    ///   is signed by that key AND the current one, so a directory cannot
    ///   attach a key of its choosing to it;
    /// - this device already holds a proven chain (a supersession proof or a
    ///   verified link) from the request's key to the current one.
    /// Anything else proves nothing and the publisher is skipped.
    static func provesSamePerson(requestKeyHex: String, bundle: IdentityFetchResponse) -> Bool {
        guard let current = Data(base64Encoded: bundle.signaturePubkey) else { return false }
        if hex(current) == requestKeyHex { return true }
        if let raw = bundle.conversationLink, let link = Data(base64Encoded: raw),
           let conversationKey = try? verifyConversationLink(link: link, trustedCurrentKey: current),
           hex(conversationKey) == requestKeyHex {
            return true
        }
        guard let requestKey = Data(channelHex: requestKeyHex) else { return false }
        return ProvenPredecessorStore().load()
            .samePerson(requestKey.base64EncodedString(), bundle.signaturePubkey)
    }

    /// A publisher's side: application messages read from the journal. An
    /// admission seed or a read key is kept only when it is addressed to this
    /// account and its sender resolved (at receipt, C-68) to the owner.
    static func receive(
        _ applications: [CoPublisher.JournalApplication], ownerAccountKey: Data?,
        channelId: String, currentSeed: Data, state: ChannelsUiState?
    ) {
        guard let ownerAccountKey, let channelBytes = ChannelsClient.hexToData(channelId) else { return }
        let fromOwner = applications.filter { $0.senderAccount == ownerAccountKey }.map(\.data)
        guard !fromOwner.isEmpty else { return }
        var identities: [Identity] = []
        if let current = try? Identity.fromSeed(seed: currentSeed) { identities.append(current) }
        if let lineage = try? ChannelPublisherIdentity.shared.identity(currentSeed: currentSeed) {
            identities.append(lineage)
        }
        for plaintext in fromOwner {
            // UX 15.2.1 — the channel's earlier seeds, addressed to this
            // account: added only for epochs with no seed held.
            if let envelope = try? decodeChannelEpochSeedsEnvelope(plaintext: plaintext) {
                guard envelope.channelId == channelBytes, let state else { continue }
                for identity in identities {
                    if let opened = try? openChannelEpochSeedsEnvelope(plaintext: plaintext, accountIdentity: identity) {
                        let keep = epochSeedsToKeep(
                            received: opened.map { (epoch: $0.epoch, seed: $0.seed) },
                            held: state.readSeeds(channelId: channelId))
                        for item in keep {
                            state.setSubscribedSeed(channelId: channelId, epoch: item.epoch, seed: item.seed)
                        }
                        break
                    }
                }
                continue
            }
            if let envelope = try? decodeChannelReadKeyEnvelope(plaintext: plaintext) {
                guard envelope.channelId == channelBytes else { continue }
                for identity in identities {
                    if let key = try? openChannelReadKeyEnvelope(plaintext: plaintext, accountIdentity: identity) {
                        state?.updateExtras(channelId) { $0.readKey = key }
                        break
                    }
                }
                continue
            }
            guard let envelope = try? decodeAdmissionSeedEnvelope(plaintext: plaintext),
                  envelope.channelId == channelBytes else { continue }
            for identity in identities {
                if let seed = try? openAdmissionSeedEnvelope(plaintext: plaintext, accountIdentity: identity) {
                    ChannelInvites.putAdmissionSeed(
                        channelIdHex: channelId, version: envelope.admissionVersion, seed: seed)
                    break
                }
            }
        }
    }

    /// Forget device / Delete account.
    nonisolated static func forget(accountIdB64: String) {
        let prefix = "me.lakko.channel-seed-share.v1.\(accountIdB64)."
        for key in UserDefaults.standard.dictionaryRepresentation().keys where key.hasPrefix(prefix) {
            UserDefaults.standard.removeObject(forKey: key)
        }
    }
}
