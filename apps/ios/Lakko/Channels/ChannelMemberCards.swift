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

    /// `channelId hex|epoch|grant` keys already uploaded this process: a card
    /// is uploaded once per epoch (a newcomer holds only current seeds) and
    /// again when the `channel` grant it carries changed (ADR-0078 section 9:
    /// the class key rotates on a channel left or a setting change).
    private static var uploaded: Set<String> = []

    /// The subscriber key and the seed this device's own card goes up with.
    struct CardSource: Equatable {
        let keys: ChannelSubscriberKeys
        let epoch: UInt32
        let seed: Data
    }

    /// Which key and seed this device uploads the account's own card with,
    /// or `nil` (live walk 2026-11-26, D1: the owner's and the publishers'
    /// cards are uploaded too, or no subscriber device can tell the owner
    /// reset and no owner return is ever planned, ADR-0101 section 7). Pure.
    ///
    /// | this device                       | key                                         | seed                         |
    /// |-----------------------------------|---------------------------------------------|------------------------------|
    /// | publishes the channel             | its subscriber key, else the account's       | the composer's (its epoch)   |
    /// |                                   | register key once registered at the service |                              |
    /// | a subscriber, or a reader device  | its subscriber key                          | the newest seed it holds     |
    /// | of the owner or a publisher       |                                             |                              |
    ///
    /// A register key the service does not list yet gets no card (the card
    /// route needs a subscriber row); the next pass uploads once it does.
    static func cardSource(
        publishing: Bool, subscriberKeys: ChannelSubscriberKeys?, registerKeys: ChannelSubscriberKeys?,
        readerKeyRegisteredHex: String?, composerSeed: (epoch: UInt32, seed: Data)?,
        heldSeeds: [UInt32: Data]
    ) -> CardSource? {
        if publishing {
            guard let composerSeed else { return nil }
            if let subscriberKeys {
                return CardSource(keys: subscriberKeys, epoch: composerSeed.epoch, seed: composerSeed.seed)
            }
            guard let registerKeys,
                  readerKeyRegisteredHex?.lowercased()
                    == ChannelsComposer.bytesToHex(registerKeys.publicKey).lowercased()
            else { return nil }
            return CardSource(keys: registerKeys, epoch: composerSeed.epoch, seed: composerSeed.seed)
        }
        guard let subscriberKeys, let newest = heldSeeds.max(by: { $0.key < $1.key }) else { return nil }
        return CardSource(keys: subscriberKeys, epoch: newest.key, seed: newest.value)
    }

    /// Upload this account's card for the newest epoch this device holds a
    /// seed for, when not done yet: a subscriber's, and (D1) the owner's and
    /// each publisher's, under the same "Members can find me" rule (the
    /// `channel` grant only where the setting lets this channel's members
    /// find the person). Best effort.
    static func uploadIfNeeded(state: ChannelsUiState, channelId: String) async {
        let publishing = state.keysByChannel[channelId] != nil
        guard let source = cardSource(
                publishing: publishing,
                subscriberKeys: state.subscriberKeysByChannel[channelId],
                registerKeys: publishing ? ChannelSubscriptionSync.shared.registeredKeys(channelId: channelId) : nil,
                readerKeyRegisteredHex: state.extras(channelId).readerKeyRegisteredHex,
                composerSeed: publishing ? state.composerSeed(channelId: channelId) : nil,
                heldSeeds: state.subscribedSeeds(channelId: channelId)),
              let channelIdBytes = CoPublisher.hexToData(channelId) else { return }
        let keys = source.keys
        let epoch = source.epoch
        let seed = source.seed
        let grant = ReachCenter.shared.channelGrant(channelId: channelIdBytes)
        // The profile's stamp is part of the key: a new name or photo is a new
        // card for the same epoch (ADR-0084).
        let stamp = ProfileDirectory.shared.ownProfile?.updatedAtMs ?? 0
        // The sharing flag too (ADR-0092 section 7): a switch to or from
        // `Nobody` changes what the card says without a new name.
        let shareable = ProfileDirectory.shared.ownCardShareable
        let uploadKey = "\(channelId)|\(epoch)|\(grant.map { String($0.version) } ?? "-")|\(stamp)|\(shareable)"
        // ADR-0101 section 8 — the profile in the card is signed by the
        // account identity (profile v6).
        guard !uploaded.contains(uploadKey),
              var profile = ProfileDirectory.shared.ownProfileForRequest(),
              let accountIdB64 = ProfileDirectory.shared.accountIdB64,
              let signer = ProfileDirectory.shared.signingIdentity(accountIdB64: accountIdB64)
        else { return }
        profile.grants = grant.map { [$0] } ?? []
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
                subscriberPk: kp.`public`(),
                profile: try composeProfileEnvelope(profile: profile, accountIdentity: signer))
            else { return }
            let sig = try kp.memberCardRequestSignature(channelId: channelIdBytes, card: card)
            try await ChannelsClient().putMemberCard(
                channelIdHex: channelId,
                subscriberPubHex: ChannelsComposer.bytesToHex(kp.`public`()),
                card: card, sigHeader: sig)
            forgetUploads(channelId: channelId)
            uploaded.insert(uploadKey)
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
        // ADR-0078 — the mailbox the card's own grant names, its binding
        // verified against the key the card's account binding verified under.
        // A grant whose binding does not verify is never checked, and never
        // filed: it answers `false`.
        guard let target = MailboxTarget.verifiedOrNil(grant: grant, accountKey: authorAccountKey) else {
            verifiedGrants[cacheKey] = false
            return false
        }
        guard let ok = try? await DsClient().reachCheck(pseudonym: target.address, reachSeed: grant.seed)
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
        await refreshMembers(state: state, channelId: channelId, resolveKey: resolveKey).map(\.claim)
    }

    /// One named member: the subscriber key the card was filed under and
    /// what it says.
    struct Member {
        let subscriberPk: Data
        let claim: ProfileClaim
        /// The account key the card is bound to (verified when it opened).
        var accountKey = Data()
        /// Every subscriber key a card of this member was stored under (one
        /// per device): what `Remove` takes in a private channel (UX 7.3).
        var subscriberPks: [Data] = []
        /// The viewer's own account ("You"), listed only on request.
        var isSelf = false

        /// The card carries something to call the person by: a display name
        /// or a username shown here. A card without one is not a named row
        /// (UX 15.1).
        var named: Bool {
            isSelf || !(claim.displayName?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true)
                || claim.username != nil
        }
    }

    /// ``refresh(state:channelId:resolveKey:)``, with the subscriber key of
    /// each name (the subscriber list's rows, ADR-0087 section 15).
    ///
    /// `includeSelf`: the subscriber list (UX 15.1) also gets one "You"
    /// member carrying this account's own subscriber keys (this device's
    /// registered key, and the keys of the account's other devices' cards),
    /// so an own key is never shown as an anonymous subscriber.
    static func refreshMembers(
        state: ChannelsUiState, channelId: String, includeSelf: Bool = false,
        resolveKey: (Data) async -> Data?
    ) async -> [Member] {
        // A publisher opens the cards of the epoch its composer seed serves
        // with that seed; every other epoch reads from the seeds kept.
        let seeds = state.readSeeds(channelId: channelId)
        guard !seeds.isEmpty, let channelIdBytes = CoPublisher.hexToData(channelId) else { return [] }
        let ownPk = state.subscriberKeysByChannel[channelId]
            .flatMap { try? ChannelSubscriberKeypair.fromSecret(secret: $0.secret).`public`() }
        let ownAccountKey = ReachCenter.shared.ownAccountKey
        // The account this read runs for, before its first wait: a card's
        // profile is filed under it, and under no account signed in while the
        // pages were being read.
        let readForAccountIdB64 = ProfileDirectory.shared.accountIdB64
        var names: [Member] = []
        var seen = Set<Data>()
        var selfPks: [Data] = ownPk.map { [$0] } ?? []
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
                          subscriberPk: member.subscriberPk) else { continue }
                let key = opened.identityPk
                // ADR-0101 section 8 — the card's profile counts only when
                // the account its binding names signed it. A card whose
                // profile is not is still a member (the binding verified),
                // drawn as an unnamed row: no name, username, photo or grant
                // is taken from it, and nothing is filed.
                guard let profile = try? verifyProfileEnvelope(plaintext: opened.profile, identityPk: key) else {
                    if key == ownAccountKey {
                        if !selfPks.contains(member.subscriberPk) { selfPks.append(member.subscriberPk) }
                    } else if let at = names.firstIndex(where: { $0.accountKey == key }) {
                        if !names[at].subscriberPks.contains(member.subscriberPk) {
                            names[at].subscriberPks.append(member.subscriberPk)
                        }
                    } else if seen.insert(key).inserted {
                        names.append(Member(
                            subscriberPk: member.subscriberPk,
                            claim: ProfileClaim(displayName: nil, username: nil, updatedAtMs: 0),
                            accountKey: key, subscriberPks: [member.subscriberPk]))
                    }
                    continue
                }
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
                guard key != ownAccountKey else {
                    if !selfPks.contains(member.subscriberPk) { selfPks.append(member.subscriberPk) }
                    continue
                }
                guard seen.insert(key).inserted else {
                    // Another device of the same member: one row, every key.
                    if let at = names.firstIndex(where: { $0.accountKey == key }),
                       !names[at].subscriberPks.contains(member.subscriberPk) {
                        names[at].subscriberPks.append(member.subscriberPk)
                    }
                    continue
                }
                let username = shownUsername(
                    username: profile.username, carriesChannelGrant: channelGrant != nil,
                    grantHeld: ReachCenter.shared.holdsGrant(
                        .channel, fromAccountKeyB64: keyB64, scopeHex: channelId))
                let claim = ProfileClaim(
                    displayName: profile.displayName, username: username,
                    updatedAtMs: profile.updatedAtMs, accountId: profile.accountId,
                    usernameCertificate: username == nil ? nil : profile.usernameCertificate,
                    avatar: ProfileAvatar(profile.avatar),
                    cardShareable: profile.cardShareable,
                    envelope: opened.profile)
                names.append(Member(
                    subscriberPk: member.subscriberPk, claim: claim, accountKey: key,
                    subscriberPks: [member.subscriberPk]))
                if channelGrant != nil {
                    ProfileDirectory.shared.acceptReceived(
                        claim, signatureKeyB64: keyB64, accountIdB64: readForAccountIdB64)
                }
            }
            after = page.nextAfter
        } while after != nil
        if includeSelf, let first = selfPks.first {
            names.insert(Member(
                subscriberPk: first, claim: ProfileClaim(displayName: nil, username: nil, updatedAtMs: 0),
                accountKey: ownAccountKey ?? Data(), subscriberPks: selfPks, isSelf: true), at: 0)
        }
        return names
    }
}
