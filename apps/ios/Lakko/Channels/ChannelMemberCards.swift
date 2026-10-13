// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0060 section 3 — channels show their members: each subscriber uploads
/// a member card (its profile v3, sealed under the channel's current epoch
/// seed) signed by its Ed25519 subscriber key, re-uploads it after every epoch
/// rotation; the server drops it with the subscription on unsubscribe
/// (``ChannelUnsubscribe``). A card carries the `channel` grant
/// only where the setting lets that channel's members find the owner.
@MainActor
enum ChannelMemberCards {

    /// `channelId hex|epoch` pairs already uploaded this process: a card is
    /// uploaded once per epoch (a newcomer holds only current seeds).
    private static var uploaded: Set<String> = []

    /// Upload this subscriber's card for the newest epoch it holds a seed for,
    /// when not done yet. Best effort.
    static func uploadIfNeeded(state: ChannelsUiState, channelId: String) async {
        guard let keys = state.subscriberKeysByChannel[channelId],
              let (epoch, seed) = state.subscribedSeeds(channelId: channelId).max(by: { $0.key < $1.key }),
              let channelIdBytes = CoPublisher.hexToData(channelId),
              !uploaded.contains("\(channelId)|\(epoch)"),
              var profile = ProfileDirectory.shared.ownProfileForRequest() else { return }
        profile.grants = ReachCenter.shared.channelGrant(channelId: channelIdBytes, epoch: epoch)
            .map { [$0] } ?? []
        if profile.grants.isEmpty {
            // Not findable by this channel's members: the name, no username.
            profile.username = nil
            profile.usernameCertificate = nil
        }
        do {
            let kp = try ChannelSubscriberKeypair.fromSecret(secret: keys.secret)
            // The card is bound to the account (ADR-0060 section 3): with no
            // active account there is no seed to sign with, and a reader
            // drops an unbound card.
            guard let card = try ReachCenter.shared.sealOwnMemberCard(
                seed: seed, epochId: epoch, channelId: channelIdBytes,
                subscriberPk: kp.`public`(), profile: try composeProfileEnvelope(profile: profile))
            else { return }
            let sig = try kp.memberCardRequestSignature(channelId: channelIdBytes, card: card)
            try await ChannelsClient().putMemberCard(
                channelIdHex: channelId,
                subscriberPubHex: ChannelsComposer.bytesToHex(kp.`public`()),
                card: card, sigHeader: sig)
            uploaded.insert("\(channelId)|\(epoch)")
            ReachCenter.shared.sync()
        } catch {
            NSLog("ADR-0060: member card not uploaded: \(error)")
        }
    }

    /// Forget which epochs were uploaded for `channelId` (the subscriber
    /// left; the server dropped the card with the subscription), so a later
    /// re-subscribe in this process uploads again.
    static func forgetUploads(channelId: String) {
        uploaded = uploaded.filter { !$0.hasPrefix("\(channelId)|") }
    }

    // MARK: - Reading the member list (the rules mirror web `member-cards.ts`)

    /// What to do with the grant state of one card read from the member list
    /// (web `cardGrantDecision`).
    enum CardGrantDecision: Equatable {
        /// File the card's `channel` grant under its author's account key.
        case file
        /// The card carries no `channel` grant: withdraw one held from before
        /// (the person narrowed their setting).
        case withdraw
        /// Leave the held grants alone.
        case skip
    }

    /// Decide the grant outcome of one card. A grant is filed only after the
    /// DS reach check showed it registered on the author's mailbox
    /// (`grantVerified == true`): a grant another member copied or made up
    /// would otherwise be filed under someone else's key. A refused grant is
    /// not the person narrowing their setting (that is a card without one),
    /// and an unanswered check proves nothing, so both leave the store alone.
    static func cardGrantDecision(
        isOwnCard: Bool, authorAccountKey: Data?, ownAccountKey: Data?,
        carriesChannelGrant: Bool, grantVerified: Bool?
    ) -> CardGrantDecision {
        if isOwnCard { return .skip }
        guard let author = authorAccountKey else { return .skip }
        if let own = ownAccountKey, author == own { return .skip }
        if !carriesChannelGrant { return .withdraw }
        return grantVerified == true ? .file : .skip
    }

    /// Whether a card whose binding verified under `cardKey` is kept, given
    /// the account key the directory publishes for the account id the card's
    /// profile claims. Nothing published (unknown account, or the lookup
    /// failed) keeps the card; only a DIFFERENT published key drops it — that
    /// card names someone else's account.
    static func cardKept(cardKey: Data, publishedKey: Data?) -> Bool {
        guard let publishedKey else { return true }
        return publishedKey == cardKey
    }

    /// The username a member row shows (UX section 6.2: the username goes
    /// with the permission): only while the card carries the `channel` grant
    /// AND this device keeps it for this channel.
    static func shownUsername(username: String?, carriesChannelGrant: Bool, grantHeld: Bool) -> String? {
        carriesChannelGrant && grantHeld ? username : nil
    }

    /// `accountKey|channel|seed` -> the reach-check answer, this process only
    /// (a relaunch re-checks once).
    private static var verifiedGrants: [String: Bool] = [:]

    /// The DS reach check of the author's mailbox with a card's grant, cached;
    /// `nil` when the check could not answer.
    private static func verifyGrant(
        _ grant: FfiReachGrant, authorAccountKey: Data, channelId: String
    ) async -> Bool? {
        let cacheKey = "\(authorAccountKey.base64EncodedString())|\(channelId)|\(grant.seed.base64EncodedString())"
        if let cached = verifiedGrants[cacheKey] { return cached }
        // ADR-0074 section 3.5a rule 1 — resolved before addressing.
        guard let mailbox = await MainActor.run(body: {
            ReachCenter.shared.mailbox(forAccountKey: authorAccountKey)
        }),
              let ok = try? await DsClient().reachCheck(pseudonym: mailbox, reachSeed: grant.seed)
        else { return nil }
        verifiedGrants[cacheKey] = ok
        return ok
    }

    /// Read the channel's member cards, open each with the seed of its epoch,
    /// and file or withdraw the `channel` grants they carry, the way web does:
    /// a card is filed under the account key its binding verified under; one
    /// whose claimed account id the directory publishes a DIFFERENT key for is
    /// dropped (nothing published keeps it); a grant is filed only once the
    /// reach check shows it registered on the author's mailbox; a card without
    /// the grant withdraws one held from before; a username is returned only
    /// while the card carries the grant and this device keeps it. Returns the
    /// names read, for a member list. Best effort.
    @discardableResult
    static func refresh(
        state: ChannelsUiState, channelId: String, resolveKey: (Data) async -> Data?
    ) async -> [ProfileClaim] {
        let seeds = state.subscribedSeeds(channelId: channelId)
        guard !seeds.isEmpty, let channelIdBytes = CoPublisher.hexToData(channelId) else { return [] }
        let ownPk = state.subscriberKeysByChannel[channelId]
            .flatMap { try? ChannelSubscriberKeypair.fromSecret(secret: $0.secret).`public`() }
        let ownAccountKey = ReachCenter.shared.ownAccountKey
        var names: [ProfileClaim] = []
        var seen = Set<Data>()
        var after: String?
        repeat {
            guard let page = try? await ChannelsClient().listMemberCards(
                channelIdHex: channelId, after: after) else { break }
            for member in page.members where member.subscriberPk != ownPk {
                // Opening verifies the card's account binding; a card that
                // does not verify throws and is dropped.
                guard let epoch = try? memberCardEpoch(card: member.card), let seed = seeds[epoch],
                      let opened = try? openMemberCard(
                          seed: seed, card: member.card, channelId: channelIdBytes,
                          subscriberPk: member.subscriberPk),
                      let profile = try? decodeProfileEnvelope(plaintext: opened.profile) else { continue }
                let key = opened.identityPk
                if let accountId = profile.accountId,
                   !cardKept(cardKey: key, publishedKey: await resolveKey(accountId)) {
                    continue
                }
                let keyB64 = key.base64EncodedString()
                let channelGrant = profile.grants.first { $0.class == .channel }
                var verified: Bool?
                if let channelGrant, key != ownAccountKey {
                    verified = await verifyGrant(channelGrant, authorAccountKey: key, channelId: channelId)
                }
                switch cardGrantDecision(
                    isOwnCard: false, authorAccountKey: key, ownAccountKey: ownAccountKey,
                    carriesChannelGrant: channelGrant != nil, grantVerified: verified) {
                case .file:
                    if let channelGrant {
                        ReachCenter.shared.keep(
                            channelGrant, carrier: .channelMemberCard,
                            fromAccountKeyB64: keyB64, scopeHex: channelId)
                    }
                case .withdraw:
                    ReachCenter.shared.withdrawHeld(.channel, fromAccountKeyB64: keyB64, scopeHex: channelId)
                case .skip:
                    break
                }
                // Every device of the own account is "you", not a member row.
                guard key != ownAccountKey, seen.insert(key).inserted else { continue }
                let username = shownUsername(
                    username: profile.username, carriesChannelGrant: channelGrant != nil,
                    grantHeld: ReachCenter.shared.holdsGrant(
                        .channel, fromAccountKeyB64: keyB64, scopeHex: channelId))
                let claim = ProfileClaim(
                    displayName: profile.displayName, username: username,
                    updatedAtMs: profile.updatedAtMs, accountId: profile.accountId,
                    usernameCertificate: username == nil ? nil : profile.usernameCertificate)
                names.append(claim)
                if channelGrant != nil {
                    ProfileDirectory.shared.acceptReceived(claim, signatureKeyB64: keyB64)
                }
            }
            after = page.nextAfter
        } while after != nil
        return names
    }
}
