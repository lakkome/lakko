// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import Security

/// ADR-0087 sections 7 and 14 — a channel's invitations: the `/g/` link of
/// preview kind 3 (a subscriber invitation), for public and private
/// channels, and the private channel's admission key.
///
/// - **Public:** the link carries the channel's read key; nothing is
///   registered at the channels service.
/// - **Private:** the link carries a fresh invite seed; its Ed25519 invite key
///   is registered at the channels service with the link's expiry and use
///   limit, and the admission public key (and version) the subscriber seals
///   its ticket to. The publisher's device opens each ticket before it lets
///   a subscriber in (``SubscriberAdmission``).
@MainActor
enum ChannelInvites {

    /// The links this device minted for a channel, with the invite key of a
    /// private one (to revoke it at the channels service).
    struct MintedLink: Codable, Equatable, Identifiable {
        let channelIdHex: String
        let slug: String
        let url: String
        let invitePublicKey: Data?
        let label: String
        let expiryMs: UInt64
        let maxUses: UInt32
        let mintedAtMs: Int64
        var id: String { slug }
    }

    enum InviteError: Error {
        case notPublisher
        case noAccess
        /// A co-publisher of a private channel holds no admission seed yet:
        /// the owner has not shared it (``ChannelAdmissionSeedShare``).
        case noAdmissionSeedYet
    }

    // MARK: - The admission key (section 7, decided 2026-11-02)

    /// The channel's newest admission seed and its version. Version 0 is
    /// minted (`mintIfMissing`) only at the two moments of
    /// ``AdmissionSeedMoment``: the creation of a private channel on this
    /// device, and a public-to-private switch made on this device. Everywhere
    /// else a device without a seed waits for the account's admission-seed
    /// register (security pass 2026-11-15, F1): an owner device that holds
    /// none for an existing private channel is a second owner device the
    /// register has not reached yet, and a co-publisher never mints (it would
    /// collide with the owner's version 0 and orphan the tickets sealed to
    /// it). Kept in the Keychain. SECRET: whoever holds it opens the
    /// channel's tickets.
    static func admissionSeed(channelIdHex: String, mintIfMissing: Bool) -> (seed: Data, version: UInt16)? {
        if let newest = admissionSeeds(channelIdHex: channelIdHex).last { return newest }
        guard mintIfMissing, let account = AcceptedContactsStore.boundAccountIdB64 else { return nil }
        var all = ChannelAdmissionKeyStore.load(accountIdB64: account)
        let entry = ChannelAdmissionKeyStore.Entry(seed: generateChannelAdmissionSeed(), version: 0)
        all[channelIdHex.lowercased()] = [entry]
        ChannelAdmissionKeyStore.save(all, accountIdB64: account)
        // ADR-0090 open question 4 — into the account's register, so another
        // owner device (and a restored one) checks the tickets of this seed.
        ChannelDeviceAdmission.syncAdmissionSeeds(channelId: channelIdHex)
        return (entry.seed, entry.version)
    }

    /// Security pass 2026-11-15 (F1) — the moments a device may mint a
    /// private channel's first admission seed, and whether it then makes the
    /// service's current subscriber rows readers (its admission mark).
    enum AdmissionSeedMoment: Equatable {
        /// New channel on this device: no genuine row exists yet, so any row
        /// the service lists was planted by it. Mint, mark nothing.
        case creation
        /// The owner switched the channel from public to private on this
        /// device: the rows of today are its subscribers (UX 4.1, "Everyone
        /// subscribed stays"). Mark them, and mint if no seed is held.
        case switchedToPrivateHere
        /// Anything else (a link, the account's reader key, a pass): a
        /// device without a seed waits for the register; it never mints and
        /// never marks.
        case later
    }

    struct AdmissionSeedAction: Equatable {
        let mint: Bool
        let markCurrentRows: Bool
    }

    nonisolated static func admissionSeedAction(
        _ moment: AdmissionSeedMoment, holdsSeed: Bool
    ) -> AdmissionSeedAction {
        switch moment {
        case .creation: return AdmissionSeedAction(mint: !holdsSeed, markCurrentRows: false)
        case .switchedToPrivateHere: return AdmissionSeedAction(mint: !holdsSeed, markCurrentRows: true)
        case .later: return AdmissionSeedAction(mint: false, markCurrentRows: false)
        }
    }

    /// New channel (private) — mint version 0 now, before anything can ask
    /// for it, and mark no row (the web creation). `nil` without a bound
    /// account.
    @discardableResult
    static func mintAtCreation(channelIdHex: String) -> (seed: Data, version: UInt16)? {
        let holds = !admissionSeeds(channelIdHex: channelIdHex).isEmpty
        return admissionSeed(
            channelIdHex: channelIdHex,
            mintIfMissing: admissionSeedAction(.creation, holdsSeed: holds).mint)
    }

    /// Every admission seed held for a channel, oldest version first: older
    /// versions stay so tickets from older links still open.
    ///
    /// ADR-0090 open question 4 — with the owner account's register: every
    /// seed of every version, from this device and from the account's synced
    /// state (two seeds of one version both kept), so a ticket of a version
    /// opens under any of them.
    static func admissionSeeds(channelIdHex: String) -> [(seed: Data, version: UInt16)] {
        mergedAdmissionSeeds(
            local: localAdmissionSeeds(channelIdHex: channelIdHex),
            register: ChannelDeviceAdmission.registerSeeds(channelId: channelIdHex))
    }

    /// The seeds this device keeps in its own store, oldest version first.
    static func localAdmissionSeeds(channelIdHex: String) -> [(seed: Data, version: UInt16)] {
        guard let account = AcceptedContactsStore.boundAccountIdB64 else { return [] }
        return (ChannelAdmissionKeyStore.load(accountIdB64: account)[channelIdHex.lowercased()] ?? [])
            .sorted { $0.version < $1.version }
            .map { ($0.seed, $0.version) }
    }

    /// The union of `local` and `register`, each seed once, oldest version
    /// first (a version may appear more than once).
    nonisolated static func mergedAdmissionSeeds(
        local: [(seed: Data, version: UInt16)], register: [(seed: Data, version: UInt16)]
    ) -> [(seed: Data, version: UInt16)] {
        var seen = Set<Data>()
        return (local + register)
            .filter { $0.seed.count == 32 && seen.insert($0.seed).inserted }
            .sorted { $0.version < $1.version }
    }

    /// A version the owner sealed to this publisher. The first seed given
    /// for a version is kept and never replaced.
    static func putAdmissionSeed(channelIdHex: String, version: UInt16, seed: Data) {
        guard let account = AcceptedContactsStore.boundAccountIdB64, seed.count == 32 else { return }
        var all = ChannelAdmissionKeyStore.load(accountIdB64: account)
        var held = all[channelIdHex.lowercased()] ?? []
        // ADR-0090 — two owner devices may mint one version at once: both
        // seeds of it are kept (each seed once).
        guard !held.contains(where: { $0.seed == seed }) else { return }
        held.append(ChannelAdmissionKeyStore.Entry(seed: seed, version: version))
        all[channelIdHex.lowercased()] = held
        ChannelAdmissionKeyStore.save(all, accountIdB64: account)
    }

    // MARK: - Links

    /// Mint a subscriber invitation link for `channelIdHex` (section 14.2):
    /// upload it to the ledger and keep it in this device's links.
    static func mintLink(
        channelIdHex: String, state: ChannelsUiState, client: ChannelsClient, currentSeed: Data,
        label: String = "", expiryMs: UInt64 = 0, maxUses: UInt32 = UInt32.max
    ) async throws -> MintedLink {
        guard state.keysByChannel[channelIdHex] != nil,
              let channelId = ChannelsClient.hexToData(channelIdHex) else { throw InviteError.notPublisher }
        let identity = try Identity.fromSeed(seed: currentSeed)
        // A channel made before phase B carries no flag (no handle block):
        // the server took it as not private, so its owner repairs it as public
        // (its read key) and the info screen asks for an @name (UX 4.4).
        ChannelInvites.repairFlagIfMissing(channelIdHex: channelIdHex, state: state, currentSeed: currentSeed)
        let extras = state.extras(channelIdHex)
        let channel = state.channels.first { $0.id == channelIdHex }
        let access: FfiChannelAccess
        var invitePk: Data?
        var inviteSeedKept = Data()
        var admissionVersion: UInt16 = 0
        if extras.isPublic == false {
            // A co-publisher uses the versions the owner sealed to it and,
            // until one arrives, mints no private link (the invite key is
            // registered only once the seed is known, below).
            // Security pass 2026-11-15 (F1) — no seed here is a seed not
            // here YET (the creation and the switch mint one): the owner's
            // device waits for the register like a co-publisher, and marks
            // no row.
            guard let admission = admissionSeed(
                channelIdHex: channelIdHex,
                mintIfMissing: admissionSeedAction(
                    .later, holdsSeed: !admissionSeeds(channelIdHex: channelIdHex).isEmpty).mint)
            else { throw InviteError.noAdmissionSeedYet }
            let inviteSeed = generateChannelInviteSeed()
            let signer = try ChannelInviteSigner.fromInviteSeed(inviteSeed: inviteSeed)
            invitePk = signer.verifyingKey()
            inviteSeedKept = inviteSeed
            admissionVersion = admission.version
            try await client.putInviteKey(
                channelIdHex: channelIdHex, invitePublicKey: signer.verifyingKey(), expiryMs: expiryMs,
                maxUses: maxUses, accountIdentity: identity)
            access = .private(
                inviteSeed: inviteSeed,
                admissionPublicKey: try channelAdmissionPublicKey(admissionSeed: admission.seed),
                admissionVersion: admission.version)
        } else {
            let readKey = extras.readKey ?? ChannelLineage.readKey(
                accountIdB64: AcceptedContactsStore.boundAccountIdB64, currentSeed: currentSeed, channelId: channelId)
            guard let readKey else { throw InviteError.noAccess }
            access = .public(readKey: readKey)
        }
        let mint = try composeChannelSubscriberInviteCapability(
            issuerSigningSeed: identity.ed25519Seed(), channelId: channelId, expiryMs: expiryMs,
            channelName: channel?.title ?? "", description: channel?.displayDescription ?? "",
            access: access, avatar: extras.avatar?.ffi,
            // Section 7.2: the link page reads "Anna invites you to"; the core
            // leaves out a name that is not valid or does not fit.
            inviterName: ProfileDirectory.shared.ownProfile?.displayName)
        let revocation = try LinkRevocationKey.fresh()
        let discovery = DiscoveryClient(sessionToken: AmbientSessionToken.shared.current)
        let blobId = try await discovery.uploadGroupInvite(
            mint: mint, revocationPubkey: revocation.publicKey, expiryMs: expiryMs, maxUses: maxUses)
        let slug = base64UrlEncodeNoPad(blobId)
        let url = InviteLinkSecret.link(
            base: try GroupInviteUrl.composePointerGroupInviteUrl(blobId: blobId, origin: CoPublisher.defaultOrigin),
            secret: mint.linkSecret)
        let minted = MintedLink(
            channelIdHex: channelIdHex.lowercased(), slug: slug, url: url, invitePublicKey: invitePk,
            label: label, expiryMs: expiryMs, maxUses: maxUses,
            mintedAtMs: Int64(Date().timeIntervalSince1970 * 1000))
        // ADR-0087 phase B — in the synced register, so every device of the
        // account lists it and any of them revokes it.
        await GroupLinkRegister.record(
            kind: .channel, ledgerId: blobId, linkSecret: mint.linkSecret, revocationSeed: revocation.seed,
            targetId: channelId, inviteSeed: inviteSeedKept, admissionVersion: admissionVersion,
            capability: mint.sealed, label: label, expiryMs: expiryMs, maxUses: maxUses)
        if let account = AcceptedContactsStore.boundAccountIdB64 {
            OwnLinkStore.shared.put(accountIdB64: account, OwnLink(
                kind: .group, slug: slug, revocationSeed: revocation.seed, label: label,
                expiryMs: Int64(clamping: expiryMs), maxUses: Int64(maxUses), bootstrapReferral: false,
                mintedAtMs: minted.mintedAtMs))
            var links = ChannelLinkStore.load(account)
            links.append(minted)
            ChannelLinkStore.save(links, account)
        }
        return minted
    }

    /// A channel this device owns whose record carries no flag (made before
    /// ADR-0087 phase B): its owner treats it as public — what the channels
    /// service took it as — by keeping its read key, so links can be minted
    /// and the public seed is published. No-op for anything else.
    static func repairFlagIfMissing(channelIdHex: String, state: ChannelsUiState, currentSeed: Data) {
        guard state.extras(channelIdHex).isPublic == nil,
              ChannelOwnership.isOwner(channelId: channelIdHex, state: state),
              let channelId = ChannelsClient.hexToData(channelIdHex),
              let readKey = ChannelLineage.readKey(
                accountIdB64: AcceptedContactsStore.boundAccountIdB64, currentSeed: currentSeed, channelId: channelId)
        else { return }
        state.updateExtras(channelIdHex) { $0.readKey = readKey }
    }

    /// The channel's subscriber links made on any device of the account
    /// (the synced register), and any this device made before the register
    /// carried them, newest first.
    static func links(channelIdHex: String) -> [MintedLink] {
        guard let account = AcceptedContactsStore.boundAccountIdB64,
              let channelId = ChannelsClient.hexToData(channelIdHex) else { return [] }
        var out: [MintedLink] = GroupLinkRegister.records(kinds: [.channel], targetId: channelId).compactMap { record in
            guard let url = GroupLinkRegister.url(record) else { return nil }
            let pk = record.inviteSeed.isEmpty
                ? nil : (try? ChannelInviteSigner.fromInviteSeed(inviteSeed: record.inviteSeed))?.verifyingKey()
            return MintedLink(
                channelIdHex: channelIdHex.lowercased(), slug: base64UrlEncodeNoPad(record.ledgerId), url: url,
                invitePublicKey: pk, label: record.label, expiryMs: record.expiryMs, maxUses: record.maxUses,
                mintedAtMs: Int64(clamping: record.mintedAtMs))
        }
        let inRegister = Set(out.map(\.slug))
        let revoked = Set((GroupLinkRegister.sync?.linkRecords ?? []).filter(\.revoked)
            .map { base64UrlEncodeNoPad($0.ledgerId) })
        out += ChannelLinkStore.load(account).filter {
            $0.channelIdHex == channelIdHex.lowercased() && !inRegister.contains($0.slug) && !revoked.contains($0.slug)
        }
        return out.sorted { $0.mintedAtMs > $1.mintedAtMs }
    }

    /// Revoke a link (section 7.2): the ledger row and, for a private one,
    /// the invite key. Someone already in stays.
    static func revoke(_ link: MintedLink, client: ChannelsClient, currentSeed: Data) async throws {
        guard let account = AcceptedContactsStore.boundAccountIdB64 else { return }
        let blobId = base64UrlDecodeNoPad(link.slug)
        // The register's seed (any device's link), else this device's own.
        let record = blobId.flatMap { GroupLinkRegister.record(ledgerId: $0) }
        let seed = record.flatMap { $0.revocationSeed.count == 32 ? $0.revocationSeed : nil }
            ?? OwnLinkStore.shared.link(accountIdB64: account, kind: .group, slug: link.slug)?.seed
        if let blobId { await GroupLinkRegister.markRevoked(ledgerId: blobId) }
        if let seed, let blobId {
            let discovery = DiscoveryClient(sessionToken: AmbientSessionToken.shared.current)
            _ = try await discovery.revokeGroupInvite(blobId: blobId, signingSeed: seed)
            OwnLinkStore.shared.remove(accountIdB64: account, kind: .group, slug: link.slug)
        }
        if let pk = link.invitePublicKey {
            let identity = try Identity.fromSeed(seed: currentSeed)
            try await client.deleteInviteKey(
                channelIdHex: link.channelIdHex, invitePublicKey: pk, accountIdentity: identity)
        }
        ChannelLinkStore.save(ChannelLinkStore.load(account).filter { $0.slug != link.slug }, account)
    }

    /// Section 14 — what a contact gets in the one-to-one: the link (the
    /// message text, so a client that draws no card shows a link) and the
    /// invitation card beside it. A private channel's is a one-use link.
    static func invitation(
        channelIdHex: String, state: ChannelsUiState, client: ChannelsClient, currentSeed: Data
    ) async throws -> (url: String, card: Data) {
        let isPrivate = state.extras(channelIdHex).isPublic == false
        let minted = try await mintLink(
            channelIdHex: channelIdHex, state: state, client: client, currentSeed: currentSeed,
            maxUses: isPrivate ? 0 : UInt32.max)
        guard let blobId = base64UrlDecodeNoPad(minted.slug),
              let secret = InviteLinkSecret.read(InviteLinkSecret.split(minted.url).fragment),
              let sealed = try await DiscoveryClient(sessionToken: AmbientSessionToken.shared.current)
                .fetchGroupInvite(blobId: blobId)
        else { throw InviteError.noAccess }
        let card = try composeChannelInviteCard(ledgerId: blobId, linkSecret: secret, sealed: sealed)
        return (minted.url, card)
    }
}

/// ADR-0087 section 7 — the per-account Keychain record of the admission
/// seeds of the channels this device publishes (or co-publishes), every
/// version held, by channel.
enum ChannelAdmissionKeyStore {
    struct Entry: Codable, Equatable {
        let seed: Data
        let version: UInt16
    }

    static let service = "me.lakko.channel-admission-keys.v2"

    static func load(accountIdB64: String) -> [String: [Entry]] {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword, kSecAttrService: service, kSecAttrAccount: accountIdB64,
            kSecReturnData: true, kSecMatchLimit: kSecMatchLimitOne,
        ]
        var out: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &out) == errSecSuccess, let data = out as? Data else { return [:] }
        return (try? JSONDecoder().decode([String: [Entry]].self, from: data)) ?? [:]
    }

    static func save(_ all: [String: [Entry]], accountIdB64: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64), let data = try? JSONEncoder().encode(all) else { return }
        defer { gate.leave() }
        delete(accountIdB64: accountIdB64)
        let add: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword, kSecAttrService: service, kSecAttrAccount: accountIdB64,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly, kSecValueData: data,
        ]
        SecItemAdd(add as CFDictionary, nil)
    }

    static func delete(accountIdB64: String) {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword, kSecAttrService: service, kSecAttrAccount: accountIdB64,
        ]
        SecItemDelete(query as CFDictionary)
    }
}

/// The channel links this device minted, per account (no secret: the
/// revocation seed lives in ``OwnLinkStore``; the link URL carries its
/// secret, as any link the person can copy again does).
enum ChannelLinkStore {
    static func key(_ accountIdB64: String) -> String { "lakko.channel-links.v1.\(accountIdB64)" }

    static func load(_ accountIdB64: String) -> [ChannelInvites.MintedLink] {
        UserDefaults.standard.data(forKey: key(accountIdB64))
            .flatMap { try? JSONDecoder().decode([ChannelInvites.MintedLink].self, from: $0) } ?? []
    }

    static func save(_ links: [ChannelInvites.MintedLink], _ accountIdB64: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        UserDefaults.standard.set(try? JSONEncoder().encode(links), forKey: key(accountIdB64))
    }

    static func delete(_ accountIdB64: String) {
        UserDefaults.standard.removeObject(forKey: key(accountIdB64))
    }
}
