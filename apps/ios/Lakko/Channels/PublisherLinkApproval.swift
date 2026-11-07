// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0087 section 15 — "the owner's device approves that person's request
/// without asking, since it named them". A co-publisher request that came by
/// a publisher link this account minted (on this device, or on another one,
/// found through the synced link register by opening the record's sealed
/// capability) is approved by the owner's device as soon as it reads it; a
/// link the register shows as revoked approves nothing; a link once used is
/// revoked on every device of the account, and its ledger row goes.
@MainActor
enum PublisherLinkApproval {
    /// A publisher link this device minted: what a request carrying it is
    /// recognised by. Public data only (the plaintext capability is what the
    /// joiner sends in its request), so plain `UserDefaults` per account.
    struct LocalLink: Codable, Equatable {
        let ledgerId: Data
        let capabilityWire: Data
        var used: Bool
        /// The use limit it was minted with (`0` on the wire is one use).
        var maxUses: UInt32 = ContactsRepository.singleUse
    }

    /// The link a request came by.
    struct Match: Equatable {
        let ledgerId: Data
        /// The register's record of it, when the register holds one.
        let record: LinkRecord?
        /// It is this device's own record.
        let local: Bool
    }

    // MARK: - The rule (pure)

    /// Whether a register record still lets someone in: not revoked, not
    /// expired, uses left (`0` on the wire is one use, `UInt32.max`
    /// unlimited). The web `isLinkLive` rule.
    nonisolated static func isLive(_ record: LinkRecord, nowMs: UInt64) -> Bool {
        if record.revoked { return false }
        if record.expiryMs != 0, nowMs >= record.expiryMs { return false }
        if record.maxUses == UInt32.max { return true }
        let limit = record.maxUses == 0 ? 1 : UInt64(record.maxUses)
        return UInt64(record.uses) < limit
    }

    /// A link names one person only when it admits one use: a limit of `0`
    /// (the wire's one use) or `1`. Anything else is shareable, and a request
    /// through it waits for a manual Approve.
    nonisolated static func isOneUse(_ maxUses: UInt32) -> Bool {
        maxUses == 0 || maxUses == 1
    }

    /// The account's publisher link a request carrying `requestCapability`
    /// came by, or `nil`: this device's own unused record of it (unless the
    /// register says it was revoked on another device), else a live register
    /// record of the channel whose sealed capability `open`s to it. Only a
    /// one-use link counts: a shareable one never admits anyone by itself.
    nonisolated static func match(
        requestCapability: Data, channelId: Data, records: [LinkRecord], local: [LocalLink], nowMs: UInt64,
        open: (LinkRecord) -> Data?
    ) -> Match? {
        let publisherRecords = records.filter { $0.kind == .publisher && $0.targetId == channelId }
        if let own = local.first(where: { $0.capabilityWire == requestCapability && !$0.used }) {
            let record = publisherRecords.first { $0.ledgerId == own.ledgerId }
            if record?.revoked == true { return nil }
            guard isOneUse(own.maxUses), record.map({ isOneUse($0.maxUses) }) ?? true else { return nil }
            return Match(ledgerId: own.ledgerId, record: record, local: true)
        }
        for record in publisherRecords
        where isLive(record, nowMs: nowMs) && isOneUse(record.maxUses) && !record.capability.isEmpty {
            if open(record) == requestCapability {
                return Match(ledgerId: record.ledgerId, record: record, local: false)
            }
        }
        return nil
    }

    // MARK: - What requests carried (in memory: knocks are re-read each session)

    private static var capabilities: [String: Data] = [:]

    private static func slot(_ groupId: Data, _ joinerKey: Data) -> String {
        groupId.base64EncodedString() + ":" + joinerKey.base64EncodedString()
    }

    static func remember(_ requests: [CoPublisher.JoinRequest], groupId: Data) {
        for request in requests {
            if let capability = request.capability { capabilities[slot(groupId, request.joinerKey)] = capability }
        }
    }

    static func capability(groupId: Data, joinerKey: Data) -> Data? {
        capabilities[slot(groupId, joinerKey)]
    }

    // MARK: - This device's links

    nonisolated static func storeKey(_ accountIdB64: String) -> String { "lakko.publisher-links.v1.\(accountIdB64)" }

    nonisolated static func localLinks(
        channelIdHex: String, accountIdB64: String? = AcceptedContactsStore.boundAccountIdB64
    ) -> [LocalLink] {
        guard let accountIdB64, let data = UserDefaults.standard.data(forKey: storeKey(accountIdB64)),
              let all = try? JSONDecoder().decode([String: [LocalLink]].self, from: data) else { return [] }
        return all[channelIdHex.lowercased()] ?? []
    }

    nonisolated private static func update(
        channelIdHex: String, accountIdB64: String?, _ change: (inout [LocalLink]) -> Void
    ) {
        guard let accountIdB64, let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var all = UserDefaults.standard.data(forKey: storeKey(accountIdB64))
            .flatMap { try? JSONDecoder().decode([String: [LocalLink]].self, from: $0) } ?? [:]
        var links = all[channelIdHex.lowercased()] ?? []
        change(&links)
        all[channelIdHex.lowercased()] = links.isEmpty ? nil : links
        if let data = try? JSONEncoder().encode(all) {
            UserDefaults.standard.set(data, forKey: storeKey(accountIdB64))
        }
    }

    nonisolated static func noteMinted(
        channelIdHex: String, ledgerId: Data, capabilityWire: Data, accountIdB64: String?
    ) {
        update(channelIdHex: channelIdHex, accountIdB64: accountIdB64) {
            $0.append(LocalLink(ledgerId: ledgerId, capabilityWire: capabilityWire, used: false))
        }
    }

    nonisolated static func noteUsed(
        channelIdHex: String, ledgerId: Data, accountIdB64: String? = AcceptedContactsStore.boundAccountIdB64
    ) {
        update(channelIdHex: channelIdHex, accountIdB64: accountIdB64) { links in
            for index in links.indices where links[index].ledgerId == ledgerId { links[index].used = true }
        }
    }

    nonisolated static func clear(accountIdB64: String) {
        UserDefaults.standard.removeObject(forKey: storeKey(accountIdB64))
    }

    // MARK: - The owner's pass

    /// Approve every pending request that came by one of the account's live
    /// publisher links, then revoke that link everywhere. A request by any
    /// other link stays pending.
    static func approveNamed(
        channelId: String, seed: Data, group: PublisherGroup, state: ChannelsUiState?
    ) async {
        guard let account = AcceptedContactsStore.boundAccountIdB64,
              let channelBytes = ChannelsClient.hexToData(channelId),
              let groupId = try? group.groupId(),
              let pending = try? group.pendingExternalJoins(), !pending.isEmpty else { return }
        for joiner in pending {
            guard let requestCapability = capability(groupId: groupId, joinerKey: joiner),
                  let found = match(
                      requestCapability: requestCapability, channelId: channelBytes,
                      records: GroupLinkRegister.sync?.linkRecords ?? [],
                      local: localLinks(channelIdHex: channelId, accountIdB64: account),
                      nowMs: UInt64(Date().timeIntervalSince1970 * 1000),
                      open: { try? GroupInviteCapability.openLink(sealed: $0.capability, linkSecret: $0.linkSecret).wire() })
            else { continue }
            do {
                try await CoPublisher.approve(
                    context: ChannelPublisherCommits.Context(
                        channelIdHex: channelId, currentSeed: seed, accountIdB64: account),
                    state: state, proposalRef: joiner)
            } catch {
                NSLog("[channels] a named publisher request was not approved yet; the next pass tries again")
                continue
            }
            capabilities[slot(groupId, joiner)] = nil
            noteUsed(channelIdHex: channelId, ledgerId: found.ledgerId, accountIdB64: account)
            await revokeUsed(found, accountIdB64: account)
        }
    }

    /// A publisher link is for one person: once used it is revoked in the
    /// register (every device stops honouring it) and its ledger row goes.
    private static func revokeUsed(_ found: Match, accountIdB64: String) async {
        await GroupLinkRegister.markRevoked(ledgerId: found.ledgerId)
        let slug = base64UrlEncodeNoPad(found.ledgerId)
        let seed = found.record.flatMap { $0.revocationSeed.count == 32 ? $0.revocationSeed : nil }
            ?? OwnLinkStore.shared.link(accountIdB64: accountIdB64, kind: .group, slug: slug)?.seed
        guard let seed else { return }
        do {
            let discovery = DiscoveryClient(sessionToken: AmbientSessionToken.shared.current)
            _ = try await discovery.revokeGroupInvite(blobId: found.ledgerId, signingSeed: seed)
            OwnLinkStore.shared.remove(accountIdB64: accountIdB64, kind: .group, slug: slug)
        } catch {
            NSLog("[channels] a used publisher link's ledger row was not deleted")
        }
    }
}
