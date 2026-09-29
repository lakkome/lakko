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

    /// Read the channel's member cards, open each with the seed of its epoch,
    /// and keep the `channel` grants they carry — filed under the account key
    /// the card's binding verified under, never under the account id the
    /// profile claims. A claimed account id the directory publishes a
    /// different key for is a card naming someone else and is dropped.
    /// Returns the names read, for a member list. Best effort.
    @discardableResult
    static func refresh(
        state: ChannelsUiState, channelId: String, resolveKey: (Data) async -> Data?
    ) async -> [ProfileClaim] {
        let seeds = state.subscribedSeeds(channelId: channelId)
        guard !seeds.isEmpty, let channelIdBytes = CoPublisher.hexToData(channelId) else { return [] }
        let ownPk = state.subscriberKeysByChannel[channelId]
            .flatMap { try? ChannelSubscriberKeypair.fromSecret(secret: $0.secret).`public`() }
        var names: [ProfileClaim] = []
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
                if let accountId = profile.accountId {
                    guard let published = await resolveKey(accountId), published == key else { continue }
                }
                names.append(ProfileClaim(
                    displayName: profile.displayName, username: profile.username,
                    updatedAtMs: profile.updatedAtMs, accountId: profile.accountId,
                    usernameCertificate: profile.usernameCertificate))
                guard !profile.grants.isEmpty else { continue }
                for grant in profile.grants {
                    ReachCenter.shared.keep(
                        grant, carrier: .channelMemberCard,
                        fromAccountKeyB64: key.base64EncodedString(), scopeHex: channelId)
                }
                ProfileDirectory.shared.acceptReceived(
                    names[names.count - 1], signatureKeyB64: key.base64EncodedString())
            }
            after = page.nextAfter
        } while after != nil
        return names
    }
}
