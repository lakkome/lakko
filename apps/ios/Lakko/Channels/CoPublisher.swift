// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0028 OQ3 — channel co-publisher management (owner side, iOS).
///
/// Swift mirror of `apps/web/src/channels/copublisher.ts` (owner half). A
/// channel's PublisherGroup (ADR-0010 Tier 1) is a vanilla MLS group, so adding
/// a *co-publisher* reuses the group join-ticket + external-join machinery
/// (ADR-0027/0028) verbatim — exactly what the OQ3 FFI surface on
/// `PublisherGroup` exposes.
///
/// **Reseal-after-add (load-bearing).** `approveExternalJoin` commits the Add,
/// which advances the PublisherGroup MLS epoch — so the channel content-key seed
/// (the epoch-keyed exporter output) CHANGES. Every existing subscriber holds the
/// OLD seed, so the owner MUST reseal the new seed to all subscribers (the
/// ADR-0010 / C-54 rotation fanout) or those subscribers can no longer decrypt.
/// `approve` does this through ``ChannelReseal`` — the same staged ADR-0030
/// fanout as `RotationCron`, but WITHOUT a second `rotate()` (the Add already
/// advanced the epoch; the reseal binds to the post-approve epoch the
/// co-publisher joins at).
///
/// **G-50.** The approve is recorded (``PublisherCommitEntry``) with the N+1
/// provider snapshot before any of its effects leaves the device, and a
/// restart resumes the effects from that record
/// (``ChannelPublisherCommits/replayAll(accountIdB64:currentSeed:state:channelsClient:ds:)``).
///
/// **ADR-0078 section 6.** A PublisherGroup id is derived from the owner's
/// account seed and the channel id, so only the owner computes it; a
/// co-publisher opens the group under the id its link conveyed, recorded per
/// channel in ``CoPublishedJournals``, and the link carries the channel id in
/// its sealed preview.
///
/// The end-to-end correctness of the reseal under a real 2-publisher topology
/// needs a live 2-publisher e2e against a deployed channels backend — deferred
/// (no deployed environment), the same posture as every other live-channel gate.
public enum CoPublisher {
    /// Path prefix every `/g/` co-publisher invite URL starts with.
    public static let urlPrefix = "/g/"
    /// Default origin Lakko shares its invite links under: the web origin of
    /// the stack this build talks to (`ApiEndpoints.webOrigin`).
    public static var defaultOrigin: String { ApiEndpoints.webOrigin }

    /// Open the persistent PublisherGroup handle of a channel this device
    /// publishes. Never creates one: a channel whose group is not in this
    /// device's channel provider is read here, not published
    /// (``CoPublisherError/notOnThisDevice``). The trap rule of ADR-0087
    /// section 12 as amended 2026-11-13: founding a group at a channel's
    /// derived id outside New channel would fork the live journal (the
    /// claim would reset it), which is what a second device of the owner
    /// would do. Only ``foundPublisherGroup(identity:accountIdB64:channelIdHex:)``
    /// creates, and only New channel calls it.
    public static func openPublisherGroup(
        identity: Identity,
        accountIdB64: String?,
        channelIdHex: String
    ) throws -> PublisherGroup {
        do {
            guard let existing = try loadPublisherGroup(identity: identity, channelIdHex: channelIdHex) else {
                throw CoPublisherError.notOnThisDevice
            }
            return existing
        } catch let error as CoPublisherError {
            throw error
        } catch {
            throw LegacyChannelV1.mapOpenError(error)
        }
    }

    /// New channel only: open, or found when absent, the PublisherGroup of a
    /// channel this device is creating now.
    ///
    /// `accountIdB64` is the account whose `identity` this is, captured when
    /// the caller's work began: a group founded here is marked for it
    /// (``markFounded(_:accountIdB64:)``).
    public static func foundPublisherGroup(
        identity: Identity,
        accountIdB64: String?,
        channelIdHex: String
    ) throws -> PublisherGroup {
        guard let channelId = Self.hexToData(channelIdHex) else {
            throw CoPublisherError.badChannelId(channelIdHex)
        }
        do {
            if let existing = try loadPublisherGroup(identity: identity, channelIdHex: channelIdHex) {
                return existing
            }
            // A co-published channel is never founded here: its group id is
            // the owner's derivation (ADR-0078 section 6).
            guard CoPublishedJournals.shared.journalIdHex(channelIdHex: channelIdHex) == nil else {
                throw CoPublisherError.notAChannelGroup
            }
            // ADR-0090 — founded with this DEVICE's bound identity under the
            // owner account seed the group id derives from.
            guard let ownerSeed = ChannelPublisherIdentity.ownerAccountEd25519Seed else {
                throw ChannelPublisherIdentity.NotBound()
            }
            let created = try PublisherGroup.create(
                identity: identity, ownerAccountSeed: ownerSeed, channelId: channelId)
            markFounded(created, accountIdB64: accountIdB64)
            return created
        } catch {
            // A channel minted under the retired v1 derivation refuses here and
            // cannot be repaired — surface the readable copy, not the raw string.
            throw LegacyChannelV1.mapOpenError(error)
        }
    }

    /// The channel's PublisherGroup from `identity`'s provider, or `nil`. A
    /// co-published channel is opened under the group id its link conveyed
    /// (``CoPublishedJournals/journalIdHex(channelIdHex:)``), an owned one
    /// under the owner's derivation (ADR-0078 section 6). Never creates.
    public static func loadPublisherGroup(
        identity: Identity,
        channelIdHex: String
    ) throws -> PublisherGroup? {
        guard let channelId = Self.hexToData(channelIdHex) else {
            throw CoPublisherError.badChannelId(channelIdHex)
        }
        if let joinedHex = CoPublishedJournals.shared.journalIdHex(channelIdHex: channelIdHex),
           let joinedId = Self.hexToData(joinedHex) {
            return try publisherGroupLoadJoined(identity: identity, groupId: joinedId, channelId: channelId)
        }
        // ADR-0090 — an owned channel: `nil` is a device with no leaf yet,
        // which reads and waits to be admitted; it never founds.
        guard let ownerSeed = ChannelPublisherIdentity.ownerAccountEd25519Seed else {
            throw ChannelPublisherIdentity.NotBound()
        }
        return try publisherGroupLoad(identity: identity, ownerAccountSeed: ownerSeed, channelId: channelId)
    }

    /// ADR-0057 obligation 7 — this device founded the PublisherGroup: its
    /// next claim may reset a journal the same root claimed before (a channel
    /// whose local PublisherGroup was lost and founded again).
    ///
    /// `accountIdB64` is the account whose channel this is, captured when the
    /// caller's work began. Refused with none named, when it is no longer the
    /// bound account (late work of account A must not leave a mark that lets
    /// the journal be reset once B is signed in), or once its session has
    /// ended.
    static func markFounded(
        _ group: PublisherGroup,
        accountIdB64: String?,
        boundAccountIdB64: String? = AcceptedContactsStore.boundAccountIdB64,
        marks: JournalFoundingMarks = .shared
    ) {
        guard let groupId = try? group.groupId(),
              let accountIdB64, accountIdB64 == boundAccountIdB64 else { return }
        marks.mark(journalIdHex: DsClient.pseudonymHex(groupId), accountIdB64: accountIdB64)
    }

    /// A publisher link just minted: its URL, the chat card that carries it,
    /// and its ledger id.
    public struct MintedPublisherLink {
        public let url: String
        public let card: Data
        public let ledgerId: Data
    }

    /// ADR-0087 section 15.2 — `Add publisher`: a co-publisher `/g/` link for
    /// ONE person, one use, over the PublisherGroup's MLS `group_id`. The
    /// person's `Accept` knocks with it; the owner's device approves that
    /// request without asking, since it named them
    /// (``PublisherLinkApproval``), and revokes the link once used.
    ///
    /// ADR-0028 amendment 2026-09-28 — the capability is sealed under a 12-byte
    /// link secret that rides the fragment, and the sealed blob is uploaded to
    /// the group-invite ledger like any `/g/` link: `<origin>/g/<id>#<secret>`.
    /// A failed upload, a network one included, mints no link and throws. The
    /// link goes to the account's link register (every device of the account
    /// recognises and revokes it) and to this device's record of its links.
    ///
    /// ADR-0078 section 6 — the joiner cannot recover the channel id from the
    /// group id any more, so the capability's sealed preview carries it (and
    /// the channel's name when known).
    public static func mintPublisherLink(
        identity: Identity,
        publisherGroup: PublisherGroup,
        channelName: String? = nil,
        origin: String = defaultOrigin,
        discovery: DiscoveryClient = DiscoveryClient(sessionToken: AmbientSessionToken.shared.current),
        accountIdB64: String?
    ) async throws -> MintedPublisherLink {
        let groupId = try publisherGroup.groupId()
        let channelId = try publisherGroup.channelId()
        // ADR-0087 section 5 — the co-publisher's request is a sealed row to
        // the PublisherGroup's knock key, which the owner derives from the
        // journal root (the owner is the only one who mints these links).
        // ADR-0090 — `identity` is this device's leaf; the root and the
        // capability's issuer are the OWNER ACCOUNT (contract item 13: the
        // issuer key is the owner account key every co-publisher device
        // records beside the group).
        let owner = try ChannelPublisherIdentity.ownerAccountIdentity()
        var root = try dsJournalRootSeed(accountIdentitySeed: owner.ed25519Seed(), journalId: groupId)
        defer { RootHandover.wipe(&root) }
        var knockSeed = try groupKnockSeed(journalRootSeed: root, version: ChatRepository.currentKnockVersion)
        defer { RootHandover.wipe(&knockSeed) }
        let mint = try composeCoPublisherInviteCapability(
            issuerSigningSeed: owner.ed25519Seed(),
            groupId: groupId,
            expiryMs: 0,
            channelId: channelId,
            channelName: channelName,
            knockPublicKey: try groupKnockPublicKey(knockSeed: knockSeed),
            knockVersion: ChatRepository.currentKnockVersion,
            inviterName: await MainActor.run { ProfileDirectory.shared.ownProfile?.displayName }
        )
        let capabilityWire = try GroupInviteCapability.openLink(sealed: mint.sealed, linkSecret: mint.linkSecret).wire()
        // ADR-0078 section 10: the ledger row carries a key minted for this
        // link alone. One use: the person named is the only one meant to ask.
        let revocation = try LinkRevocationKey.fresh()
        let blobId = try await discovery.uploadGroupInvite(
            mint: mint, revocationPubkey: revocation.publicKey, expiryMs: 0,
            maxUses: ContactsRepository.singleUse)
        PublisherLinkApproval.noteMinted(
            channelIdHex: ChannelsComposer.bytesToHex(channelId), ledgerId: blobId,
            capabilityWire: capabilityWire, accountIdB64: accountIdB64)
        // ADR-0087 phase B — a publisher link in the synced register.
        await GroupLinkRegister.record(
            kind: .publisher, ledgerId: blobId, linkSecret: mint.linkSecret,
            revocationSeed: revocation.seed, targetId: channelId, capability: mint.sealed,
            maxUses: ContactsRepository.singleUse)
        if let accountIdB64 {
            OwnLinkStore.shared.put(accountIdB64: accountIdB64, OwnLink(
                kind: .group, slug: base64UrlEncodeNoPad(blobId), revocationSeed: revocation.seed,
                label: "", expiryMs: 0, maxUses: Int64(ContactsRepository.singleUse), bootstrapReferral: false,
                mintedAtMs: Int64(Date().timeIntervalSince1970 * 1000)))
        }
        let url = InviteLinkSecret.link(
            base: try GroupInviteUrl.composePointerGroupInviteUrl(blobId: blobId, origin: origin),
            secret: mint.linkSecret)
        let card = try composeChannelInviteCard(ledgerId: blobId, linkSecret: mint.linkSecret, sealed: mint.sealed)
        return MintedPublisherLink(url: url, card: card, ledgerId: blobId)
    }

    // MARK: - ADR-0053 / ADR-0057 journal access

    /// The auth for a request to the PublisherGroup's journal (contract point
    /// 1): its current-epoch access signer, and the root's lazy claim as the
    /// retry.
    public static func auth(
        ds: DsClient,
        identity: Identity,
        publisherGroup: PublisherGroup
    ) -> DsAuth? {
        guard let signer = try? publisherGroup.dsAccessSigner(identity: identity) else { return nil }
        return DsAuth(
            signer: signer,
            reclaim: { await lazyClaim(ds: ds, identity: identity, publisherGroup: publisherGroup) }
        )
    }

    /// The PublisherGroup journal's root: the channel owner's ACCOUNT identity
    /// (channels run on the account identity).
    static func rootSigner(identity: Identity, publisherGroup: PublisherGroup) throws
        -> DsStatementSignerProtocol {
        // ADR-0090 — the root is the owner ACCOUNT's (every owner device
        // derives the same one), never the device leaf `identity`.
        try dsJournalRootSigner(
            accountIdentity: try ChannelPublisherIdentity.ownerAccountIdentity(),
            journalId: try publisherGroup.groupId())
    }

    /// Claim on open. A PublisherGroup this device just founded claims with
    /// the root and resets on a `409` (obligation 7); otherwise the claim is
    /// the idempotent re-send and a refusal is left alone. Best effort.
    ///
    /// Only the CHANNEL OWNER claims. A co-publisher's root is derived from
    /// its own account seed, so its claim would squat the journal against the
    /// owner; a co-publisher device (``CoPublishedJournals``) never claims, and
    /// when the journal is still unclaimed it waits for the owner.
    @discardableResult
    public static func claimJournal(
        ds: DsStatementSending,
        identity: Identity,
        publisherGroup: PublisherGroup,
        marks: JournalFoundingMarks = .shared,
        coPublished: CoPublishedJournals = .shared
    ) async -> DsRequestSigner? {
        guard let signer = try? publisherGroup.dsAccessSigner(identity: identity),
              let groupId = try? publisherGroup.groupId() else { return nil }
        let hex = DsClient.pseudonymHex(groupId)
        guard !coPublished.contains(journalIdHex: hex) else {
            NSLog("ADR-0057: co-publisher device; the channel owner claims the journal")
            return signer
        }
        guard let root = try? rootSigner(identity: identity, publisherGroup: publisherGroup)
        else { return nil }
        do {
            if marks.isMarked(journalIdHex: hex) {
                try await JournalOwnership.claimOrReset(
                    root: root, journalId: groupId, epoch: signer.epoch(),
                    epochKey: signer.verifyingKey(), ds: ds)
                marks.clear(journalIdHex: hex)
            } else {
                try await JournalOwnership.claim(
                    root: root, journalId: groupId, epoch: signer.epoch(),
                    epochKey: signer.verifyingKey(), ds: ds)
            }
        } catch DsStatementError.conflict {
            // Already claimed by this root at another chain; commits register
            // under the root.
        } catch {
            NSLog("ADR-0057: PublisherGroup journal claim failed, retried on the next open: %@",
                  String(describing: error))
        }
        return signer
    }

    /// The root's lazy claim after a `401` (obligation 2) — the channel
    /// owner's only: a co-publisher device never claims (its root would squat
    /// the journal), it waits. An already-claimed journal (`409`) is left
    /// alone.
    static func lazyClaim(
        ds: DsStatementSending,
        identity: Identity,
        publisherGroup: PublisherGroup,
        coPublished: CoPublishedJournals = .shared
    ) async -> DsRequestSigner? {
        guard let signer = try? publisherGroup.dsAccessSigner(identity: identity),
              let groupId = try? publisherGroup.groupId() else { return nil }
        let outcome = await JournalOwnership.lazyClaim(
            isOwner: !coPublished.contains(journalIdHex: DsClient.pseudonymHex(groupId)),
            root: { try rootSigner(identity: identity, publisherGroup: publisherGroup) },
            journalId: groupId, epoch: signer.epoch(), epochKey: signer.verifyingKey(), ds: ds)
        switch outcome {
        case .claimed: return signer
        case .alreadyClaimed, .notOwner, .failed: return nil
        }
    }

    /// ADR-0081 decision 1 over obligation 5 — register the epoch a
    /// PublisherGroup commit moves to under the root AND append the commit
    /// `row`, in one request proven by `proof` (the key of the epoch the
    /// commit leaves). Throws: a refused commit aborts it.
    @discardableResult
    static func commitEpoch(
        ds: DsCommitSending,
        identity: Identity,
        publisherGroup: PublisherGroup,
        next: DsGroupSigner,
        proof: DsRequestSigner,
        row: Data
    ) async throws -> DsCommitAnswer {
        try await JournalOwnership.commitEpoch(
            authority: .root(try rootSigner(identity: identity, publisherGroup: publisherGroup)),
            journalId: try publisherGroup.groupId(), epoch: next.epoch(),
            epochKey: next.verifyingKey(), revoke: [], row: row, proof: proof, ds: ds)
    }

    /// What one ``pollJournal(ds:identity:publisherGroup:knockAfter:)`` did.
    public struct PollOutcome: Equatable {
        /// Pending co-publisher requests after the poll.
        public let pendingCount: Int
        /// Where the next knock read starts.
        public let knockCursor: Int64
        /// Where the next journal read starts.
        public var journalCursor: Int64 = 0
        /// The application messages read, with the account each sender leaf
        /// resolved to at receipt (C-68): an admission seed or a read key the
        /// owner sealed to a publisher, a publisher's step-down.
        public var applications: [JournalApplication] = []
        /// The account key of the channel's owner as this group shows it:
        /// the founder's leaf, which a removal and a step-down never take.
        public var ownerAccountKey: Data?
        /// The requests retained by this poll: the joiner key and the account
        /// id its profile carried (for the admission seeds).
        public var requests: [JoinRequest] = []
        /// ADR-0090 section 4 — the DS row time of the last commit this poll
        /// applied (the start of the group's current epoch), if any.
        public var lastCommitAtMs: Int64?
    }

    /// One application message read from the journal.
    public struct JournalApplication: Equatable {
        public let data: Data
        public let senderAccount: Data?
        /// The DS row's server time (ms), when it parses.
        public var createdAtMs: Int64? = nil
    }

    /// One co-publisher request retained by a poll.
    public struct JoinRequest: Equatable {
        /// The joiner's leaf key (the approve's `proposalRef`).
        public let joinerKey: Data
        /// The account key the KeyPackage is bound to (the joiner key for a
        /// key with no binding).
        public let accountKey: Data
        /// The account id the request's profile carried.
        public let accountId: Data?
        /// The plaintext capability of the link the request came by: a
        /// publisher link the account minted names the person, so the owner's
        /// device approves it without asking (ADR-0087 section 15).
        public var capability: Data? = nil
    }

    /// Most journal pages read per poll (100 rows a page on the DS).
    static let maxJournalPages = 50

    /// Poll the PublisherGroup's DS journal (co-publisher commits) and its
    /// KNOCKS (external-join proposals from prospective co-publishers, ADR-0053
    /// point 7). Both reads are signed. Journal rows go through the role-gated
    /// path; knock rows go through the core's knock gate
    /// (`PublisherGroup.processKnock`), which refuses anything that is not a
    /// stranger's external-join proposal for this group before processing it —
    /// a refused row is dropped and the cursor still advances. Our own
    /// already-merged commits throw and are skipped. (ADR-0057: no
    /// heal-on-apply; the committer registered the epoch.)
    public static func pollJournal(
        ds: DsClient,
        identity: Identity,
        publisherGroup: PublisherGroup,
        knockAfter: Int64 = 0,
        readKnocks: Bool = true,
        after: Int64 = 0
    ) async throws -> PollOutcome {
        let groupId = try publisherGroup.groupId()
        // From the caller's cursor, paged to the end: the DS answers 100 rows
        // a page, and a read that always started at 0 never reached a commit
        // or an admission seed past the first page once the journal outgrew
        // it (found on Android, 2026-11-05). The cursor is the caller's, in
        // memory: after a relaunch the journal is read from the start again,
        // which is safe (an applied commit is skipped).
        var cursor = after
        var applications: [JournalApplication] = []
        var lastCommitAtMs: Int64?
        for _ in 0..<maxJournalPages {
            let raw = try await ds.fetchGroup(
                groupId: groupId, after: cursor, waitSecs: 0,
                auth: auth(ds: ds, identity: identity, publisherGroup: publisherGroup))
            for m in raw.messages {
                cursor = max(cursor, m.id)
                do {
                    let processed = try publisherGroup.processIncomingVerifyingRoles(
                        identity: identity, bytes: m.payload)
                    if case .application(let data, _, let sender) = processed {
                        applications.append(JournalApplication(
                            data: data, senderAccount: sender,
                            createdAtMs: DsTimestamp.parseMs(m.createdAt)))
                    } else if case .commit = processed {
                        lastCommitAtMs = DsTimestamp.parseMs(m.createdAt) ?? lastCommitAtMs
                    }
                } catch {
                    // A commit we have already merged (epoch mismatch) — skip.
                }
            }
            if !raw.hasMore || raw.messages.isEmpty { break }
        }
        var requests: [JoinRequest] = []
        var knockCursor = knockAfter
        // `readKnocks` is the caller's floor (``KnockPollThrottle``): the
        // journal poll runs every few seconds, the knock read far less often.
        if readKnocks {
            let knocks = try await ds.fetchKnocks(
                groupId: groupId, after: knockAfter, waitSecs: 0,
                auth: auth(ds: ds, identity: identity, publisherGroup: publisherGroup))
            let channelId = try publisherGroup.channelId()
            for row in knocks.messages {
                knockCursor = max(knockCursor, row.id)
                // ADR-0059 — a co-publisher binding rides beside the proposal;
                // it is not an MLS message, so it is read here and never
                // processed.
                if let binding = try? decodeChannelCopublisherBinding(bytes: row.payload) {
                    if binding.channelId == channelId {
                        await CoPublisherKeyBook.shared.record(
                            groupId: groupId, binding: binding)
                    }
                    continue
                }
                // ADR-0087 section 5 — a sealed row to the PublisherGroup's
                // knock key (web and Android co-publishers send these). The
                // owner derives the seed of the row's version from the
                // journal root; only a row carrying a link of this group is
                // kept. Receive side only; this client still knocks with the
                // proposal form.
                if let version = knockRowVersion(row: row.payload) {
                    if let request = await retainSealedKnock(
                        identity: identity, publisherGroup: publisherGroup, groupId: groupId,
                        version: version, row: row.payload) {
                        requests.append(request)
                    }
                    continue
                }
                _ = try? publisherGroup.processKnock(identity: identity, bytes: row.payload)
            }
            // ADR-0060 section 6 — a co-publisher's attribution carries its
            // `reply` grant, the only key its closed mailbox takes the Welcome
            // under. A co-publisher knocks with its account identity, so the
            // joiner key IS its account key.
            for row in knocks.messages {
                guard let attr = try? parseGroupJoinAttribution(bytes: row.payload),
                      attr.groupId == groupId else { continue }
                await MainActor.run {
                    _ = ReachCenter.shared.keep(
                        attr.replyGrant, carrier: .request,
                        fromAccountKeyB64: base64Encode(attr.joinerSignatureKey), scopeHex: nil)
                }
            }
        }
        return PollOutcome(
            pendingCount: try publisherGroup.pendingExternalJoins().count,
            knockCursor: knockCursor,
            journalCursor: cursor,
            applications: applications,
            ownerAccountKey: ownerAccountKey(publisherGroup),
            requests: requests,
            lastCommitAtMs: lastCommitAtMs)
    }

    /// The account key at the founder's leaf (the lowest leaf index): the
    /// owner, whose leaf a removal or a step-down never takes.
    static func ownerAccountKey(_ group: PublisherGroup) -> Data? {
        // ADR-0090 — recorded beside the group (the owner's seed on an owner
        // device, the Welcome sender's account on a co-publisher's).
        try? group.ownerAccountKey()
    }

    /// Open one sealed co-publisher request and retain its KeyPackage.
    /// Anything that does not open, names no link of this group, or is not
    /// admissible is dropped.
    private static func retainSealedKnock(
        identity: Identity, publisherGroup: PublisherGroup, groupId: Data, version: UInt16, row: Data
    ) async -> JoinRequest? {
        guard let owner = try? ChannelPublisherIdentity.ownerAccountIdentity(),
              var root = try? dsJournalRootSeed(
            accountIdentitySeed: owner.ed25519Seed(), journalId: groupId) else { return nil }
        defer { RootHandover.wipe(&root) }
        guard var seed = try? groupKnockSeed(journalRootSeed: root, version: version) else { return nil }
        defer { RootHandover.wipe(&seed) }
        guard let request = try? openKnockRow(knockSeed: seed, row: row),
              let capability = request.capability,
              let cap = try? GroupInviteCapability.decodeAndVerify(wire: capability),
              cap.groupId() == groupId,
              let kept = try? publisherGroup.retainJoinRequest(
                  identity: identity, keyPackage: request.keyPackage) else { return nil }
        switch kept.outcome {
        case .retained, .alreadyPending:
            let account = ((try? keyPackageAccountKey(keyPackage: request.keyPackage)) ?? nil)
                ?? kept.joinerSignatureKey
            await MainActor.run {
                _ = ReachCenter.shared.keep(
                    request.replyGrant, carrier: .request,
                    fromAccountKeyB64: base64Encode(account), scopeHex: nil)
            }
            return JoinRequest(
                joinerKey: kept.joinerSignatureKey, accountKey: account,
                accountId: request.profile?.accountId, capability: capability)
        case .queueFull, .alreadyMember, .notAdmissible:
            return nil
        }
    }

    /// The pending co-publisher external-join requests — each a 32-byte joiner
    /// leaf signature key (the `proposalRef` for approve / reject).
    public static func pending(publisherGroup: PublisherGroup) throws -> [Data] {
        try publisherGroup.pendingExternalJoins()
    }

    /// Approve a pending co-publisher (ADR-0090: by any owner device).
    /// STAGES the Add (the group stays at epoch N), records it with the
    /// epoch-N proof seed, sends it through the commit route, and only once
    /// the DS took it merges and, in order: sends the Welcome to the joiner's
    /// account mailbox, RESEALS the new-epoch seed to every subscriber, and
    /// appoints the newcomer's co-publisher key (ADR-0059). A `409` (another
    /// owner device's commit took the epoch) abandons it: no Welcome, no
    /// reseal, and the request stays retained for the next pass.
    @MainActor
    public static func approve(
        context: ChannelPublisherCommits.Context,
        state: ChannelsUiState?,
        proposalRef: Data
    ) async throws {
        let current = try await context.channelsClient.fetchEpoch(channelIdHex: context.channelIdHex)
        let serverTarget = current &+ 1
        guard serverTarget > current else { throw CoPublisherError.unexpectedServerEpoch(current) }
        let result = try await ChannelPublisherCommits.stageAndRun(
            context, state: state, kind: .approve, joinerKey: proposalRef,
            serverTargetEpoch: serverTarget, serverEpoch: current
        ) { identity, group in
            let added = try group.stageApproveExternalJoin(identity: identity, proposalRef: proposalRef)
            return .init(commit: added.commit, welcome: added.welcome)
        }
        // Lost to another owner device's commit: the request stays retained
        // (an abandoned approve keeps it) and is approved again next pass.
        guard result.landed else { throw CoPublisherError.commitLost }
        // ADR-0051 section 4 — the record and ban list under the new epoch,
        // the only one the newcomer can open.
        if let state {
            await ChannelPublisherLoops.shared.afterRotation(
                channelId: context.channelIdHex, epoch: serverTarget, channels: state,
                client: context.channelsClient, seed: context.currentSeed)
        }
        // ADR-0087 section 7 — the newcomer is a publisher the owner's pass
        // seals the admission seeds to.
        ChannelAdmissionSeedShare.notePublisher(channelId: context.channelIdHex, joinerKey: proposalRef)
        if let (_, group) = try? context.open() {
            CoPublisherKeyBook.notePublisher(
                channelIdHex: context.channelIdHex,
                joinerKey: accountKey(ofLeaf: proposalRef, in: group) ?? proposalRef)
        }
    }

    /// The account a leaf of `group` resolves to (its ADR-0050 binding), or
    /// `nil` when no member has that leaf key.
    static func accountKey(ofLeaf leaf: Data, in group: PublisherGroup) -> Data? {
        guard let member = (try? group.members())?.first(where: { $0.signaturePublicKey == leaf }) else {
            return nil
        }
        return member.accountKey.isEmpty ? member.signaturePublicKey : member.accountKey
    }

    /// ADR-0087 section 15 (phase B), ADR-0090 — an owner device removes a
    /// publisher: every leaf of `accountKey`, STAGED, recorded, sent through
    /// the commit route; once the DS took it the commit merges, the
    /// appointment is revoked and every subscriber resealed to the new seed,
    /// which the removed person cannot derive. Lost to another owner
    /// device's commit: abandoned, nothing sent; the next pass tries again.
    @MainActor
    public static func removePublisher(
        context: ChannelPublisherCommits.Context,
        state: ChannelsUiState?,
        accountKey: Data
    ) async throws {
        let current = try await context.channelsClient.fetchEpoch(channelIdHex: context.channelIdHex)
        let serverTarget = current &+ 1
        guard serverTarget > current else { throw CoPublisherError.unexpectedServerEpoch(current) }
        let result = try await ChannelPublisherCommits.stageAndRun(
            context, state: state, kind: .removal, joinerKey: accountKey,
            serverTargetEpoch: serverTarget, serverEpoch: current
        ) { identity, group in
            .init(commit: try group.stageRemovePublisher(identity: identity, accountKey: accountKey), welcome: nil)
        }
        guard result.landed else { throw CoPublisherError.commitLost }
        if let state {
            await ChannelPublisherLoops.shared.afterRotation(
                channelId: context.channelIdHex, epoch: serverTarget, channels: state,
                client: context.channelsClient, seed: context.currentSeed)
        }
        if let (identity, group) = try? context.open() {
            refreshPublishers(identity: identity, group: group, channelIdHex: context.channelIdHex)
        }
        // Nothing more is sealed to them; approved again, they get a fresh
        // hand-over.
        ChannelAdmissionSeedShare.forgetPublisher(channelId: context.channelIdHex, accountKey: accountKey)
    }

    /// ADR-0087 section 15 — a co-publisher leaves as publisher: the
    /// step-down message on the PublisherGroup journal, which the owner's
    /// device applies as a removal of the sender's account.
    public static func stepDown(ds: DsClient, identity: Identity, publisherGroup: PublisherGroup) async throws {
        let message = try publisherGroup.stepDown(identity: identity)
        // The send ratchet moved: on disk before the message leaves (G-34).
        try await MainActor.run { try ChannelPublisherIdentity.shared.persistOrThrow() }
        _ = try await ds.publishGroup(
            groupId: try publisherGroup.groupId(), payload: message,
            auth: auth(ds: ds, identity: identity, publisherGroup: publisherGroup))
    }

    /// The publishers other than this account, from the group's members, into
    /// ``CoPublisherKeyBook``'s record (what `hasOtherPublishers` reads).
    @MainActor
    static func refreshPublishers(identity: Identity, group: PublisherGroup, channelIdHex: String) {
        guard let members = try? group.members() else { return }
        // ADR-0090 — leaves are devices: this ACCOUNT's leaves (every device
        // of it) are not "other publishers".
        let own = identity.accountPublicKey() ?? identity.signaturePublicKey()
        let others = Set(members.map { $0.accountKey.isEmpty ? $0.signaturePublicKey : $0.accountKey }
            .filter { $0 != own }
            .map { $0.map { String(format: "%02x", $0) }.joined() })
        CoPublisherKeyBook.setPublishers(others, channelIdHex: channelIdHex)
    }

    /// Write a co-publisher's Welcome to its mailbox. The joiner's
    /// external-join proposal carried a KeyPackage under `joinerKey` (its
    /// account signature key — a co-publisher joins as its account).
    /// ADR-0060 — written with the `reply` grant its attribution carried
    /// (kept by ``ReachCenter``); the mailbox is closed to anything else.
    /// ADR-0074 section 3.5a rule 1 — resolved before addressing. Rule 6 —
    /// a refused Welcome re-checks the joiner's address once. ADR-0078 — the
    /// mailbox is the verified one the reply grant's binding names; the
    /// Welcome is one sealed row of kind `welcomeRendezvous`.
    static func deliverWelcome(ds: DsClient, welcome: Data, joinerKey: Data) async throws {
        let resolve: () async throws -> MailboxWritePlan = {
            let plan = await MainActor.run {
                ReachCenter.shared.writePlan(toAccountKey: joinerKey)
            }
            guard let plan else {
                NSLog("ADR-0078: no verified mailbox for the co-publisher; its Welcome was not written")
                throw MailboxUnknown()
            }
            return plan
        }
        func publish(_ plan: MailboxWritePlan) async throws {
            let row = try plan.target.seal(kind: .welcomeRendezvous, body: welcome)
            _ = try await RefusedMailboxWrite.tryingCandidates(plan.candidates) { auth in
                try await ds.publishMailbox(
                    pseudonym: plan.target.address, payload: row, writeAuth: auth)
            }
        }
        let first = try await resolve()
        do {
            try await publish(first)
        } catch let error where RefusedMailboxWrite.isRefusal(error) {
            await PersonAddressRecheck.recheck(accountKey: joinerKey)
            let second = try? await resolve()
            guard RefusedMailboxWrite.retries(first: first, rechecked: second), let second else {
                throw error
            }
            try await publish(second)
        }
    }

    /// Reject a pending co-publisher — no commit, no epoch advance. The
    /// retained request set is part of the PublisherGroup's persisted MLS
    /// storage, so the core takes the identity to write the durable copy and
    /// the rejection survives a restart.
    public static func reject(
        identity: Identity,
        publisherGroup: PublisherGroup,
        proposalRef: Data
    ) throws {
        try publisherGroup.rejectExternalJoin(identity: identity, proposalRef: proposalRef)
        let groupId = try publisherGroup.groupId()
        Task { await CoPublisherKeyBook.shared.forget(groupId: groupId, joinerKey: proposalRef) }
    }

    // MARK: - Joiner side — a prospective co-publisher requests + joins

    /// Handles a `requestToCoPublish` returns so the caller can drive the poll
    /// loop (`pollForCoPublishWelcome`).
    public struct CoPublishRequest {
        /// The PublisherGroup's MLS `group_id` (the journal channel to poll).
        public let groupId: Data
        /// The 32-char lowercase-hex channel id the link's sealed preview
        /// carried (ADR-0078 section 6).
        public let channelIdHex: String
        /// This device's leaf signature key (ADR-0090: the DEVICE identity the
        /// knocked KeyPackage was made with).
        public let joinerKey: Data
        /// ADR-0090 — the capability's issuer key: the owner's ACCOUNT key the
        /// joined group records, and the only account whose devices may send
        /// the Welcome.
        public var ownerAccountKey: Data = Data()
    }

    /// ADR-0028 OQ3 — joiner side: request to become a co-publisher of a channel
    /// from a `/g/` co-publisher link. Decodes + verifies the capability,
    /// reads the channel id from its sealed preview (ADR-0078 section 6: the
    /// group id is derived from the owner's seed and cannot be inverted),
    /// then knocks (ADR-0053): a probe learns the
    /// registered epoch, an external-join proposal (ADR-0027) is built for it,
    /// and a second knock carries it with a self-signed join-attribution (OQ2).
    /// The caller then polls with `pollForCoPublishWelcome`.
    ///
    /// `accountIdB64` is the account whose `identity` this is, captured when
    /// the request began: the co-publisher mark is filed under it, and the
    /// request is not sent once another account is bound.
    ///
    /// `identity` is the durable channel provider's
    /// (``ChannelPublisherIdentity``, as on Android). The proposal's
    /// KeyPackage private keys are put in it, and they are the only thing
    /// that opens the Welcome the owner sends after approving. They are made
    /// durable before the knock that carries the proposal
    /// (`persistProposalKeys`, fail closed: a request whose keys are not on
    /// disk is never sent), so a request the owner approved after the app
    /// was closed still opens on the next attempt.
    public static func requestToCoPublish(
        ds: DsClient,
        identity: Identity,
        accountIdB64: String?,
        link: String,
        discovery: DiscoveryClient = DiscoveryClient(),
        persistProposalKeys: @escaping @MainActor () throws -> Void = {
            try ChannelPublisherIdentity.shared.persistOrThrow()
        },
        // ADR-0045 decision 2, ADR-0087 section 15 — the request has no
        // deadline: it is recorded durably BEFORE the knock, so the mailbox
        // loop can land the owner's Welcome whenever it arrives. A throw here
        // sends no knock.
        recordRequest: @escaping @MainActor (CoPublishRequest) throws -> Void = { _ in }
    ) async throws -> CoPublishRequest {
        // ADR-0028 amendment 2026-09-28 — `<origin>/g/<id>#<secret>`: the
        // sealed capability is fetched from the ledger and opened under the
        // fragment's secret. An inline slug (the form removed on 2026-10-19)
        // is not a co-publisher link.
        let parts = InviteLinkSecret.split(link)
        guard let idx = parts.base.range(of: urlPrefix, options: .backwards),
              case .pointer(let blobId) = try? GroupInviteUrl.parseGroupInviteSlug(
                String(parts.base[idx.upperBound...])) else {
            throw CoPublisherError.notACoPublisherLink
        }
        guard let secret = InviteLinkSecret.read(parts.fragment) else {
            throw CoPublisherError.linkDamaged
        }
        guard let sealed = try await discovery.fetchGroupInvite(blobId: blobId) else {
            throw CoPublisherError.notACoPublisherLink
        }
        let cap: GroupInviteCapability
        do {
            cap = try GroupInviteCapability.openLink(sealed: sealed, linkSecret: secret)
        } catch {
            throw CoPublisherError.linkDamaged
        }
        let wire = cap.wire()
        let groupId = cap.groupId()
        // ADR-0078 section 6 — the channel id rides in the capability's
        // sealed preview, opened under the same link secret. No channel id:
        // a link minted under a retired derivation, or a `/g/` link to a chat
        // group, each with its own copy.
        guard let channelIdBytes = (try? cap.openPreview(linkKey: secret))??.channelId,
              channelIdBytes.count == 16 else {
            if LegacyChannelV1.isLegacyV1PublisherGroupId(groupId) {
                throw LegacyChannelError(kind: .inviteLink)
            }
            throw CoPublisherError.notAChannelGroup
        }
        let channelIdHex = ChannelsComposer.bytesToHex(channelIdBytes)
        // ADR-0090 — `identity` is this device's leaf; the co-publisher binding
        // names the ACCOUNT (one appointment per account, every device of it).
        let account = try ChannelPublisherIdentity.ownerAccountIdentity()
        let binding = try composeChannelCopublisherBinding(
            accountIdentity: account, channelId: channelIdBytes)
        // ADR-0057 — this device asks to CO-publish, so it is not the channel
        // owner: it must never claim the PublisherGroup journal. Recorded
        // before the knock, fail closed: an unrecorded request could later
        // let this account's claim squat the owner's journal.
        // ADR-0078 section 6: the channel -> group id pair is recorded with
        // it, since this account cannot derive the owner's group id.
        try CoPublishedJournals.shared.markOrEnd(
            journalIdHex: DsClient.pseudonymHex(groupId), channelIdHex: channelIdHex,
            accountIdB64: accountIdB64)
        // ADR-0087 section 15 (phase B) — the request is a SEALED knock row:
        // a fresh KeyPackage, the `reply` grant the owner writes the Welcome
        // with (ADR-0060 section 6), this account's profile and the link's
        // plaintext capability, sealed to the knock key the link names. It
        // carries no epoch, so it waits through every commit. The ADR-0059
        // binding (this account's co-publisher key for the channel) rides in
        // the same knock. One knock, one `ds-knock` token.
        guard let knockKey = cap.knock() else {
            throw CoPublisherError.notACoPublisherLink
        }
        let replyGrant = try await ReachCenter.shared.newReplyGrant(
            purpose: ReplyPurpose.groupJoin(groupId))
        let keyPackage = try identity.createKeyPackage()
        // ADR-0077 decision 5 — the KeyPackage's private keys are the only
        // thing that opens the Welcome: durable BEFORE the knock, fail closed.
        guard let account = accountIdB64, account == AcceptedContactsStore.boundAccountIdB64 else {
            throw StaleBindingError()
        }
        try await persistProposalKeys()
        let row = try sealKnockRow(
            knockPublicKey: knockKey.publicKey,
            version: knockKey.version,
            keyPackage: keyPackage,
            replyGrant: replyGrant,
            profile: await MainActor.run { ProfileDirectory.shared.ownDeviceEnvelope() } ?? Data(),
            capability: wire)
        let request = CoPublishRequest(
            groupId: groupId,
            channelIdHex: channelIdHex,
            joinerKey: identity.signaturePublicKey(),
            ownerAccountKey: cap.issuerSignaturePubkey()
        )
        try await recordRequest(request)
        _ = try await ds.knock(groupId: groupId, payloads: [row, binding])
        return request
    }

    /// ADR-0028 OQ3 — joiner side: look for the Welcome the owner sent after
    /// approving, join from it, and mint local publisher keys so this device
    /// can now publish to the channel. The joined group lands at the
    /// post-approve epoch — the same one the subscribers were resealed to — so
    /// the derived `channelSeed` matches. Returns the keys on success, or `nil`
    /// if no Welcome has arrived yet (keep polling).
    ///
    /// ADR-0053 point 8 — the owner sends the Welcome to the mailbox named by
    /// this device's `joinerKey`, which it reads signed under `identity` (the
    /// key the mailbox is named after). The whole mailbox is scanned on each
    /// attempt: other Welcomes there (chat adds) do not open under this
    /// identity's co-publisher KeyPackage and are skipped.
    public static func pollForCoPublishWelcome(
        ds: DsClient,
        identity: Identity,
        accountIdB64: String?,
        groupId: Data,
        joinerKey: Data,
        channelIdHex: String,
        ownerAccountKey: Data,
        // G-29 — reuse the persisted per-channel signing seed if this device
        // has co-published to the channel before (nil on a first join).
        existingSigningSeed: Data? = nil
    ) async throws -> ChannelPublisherKeys? {
        // ADR-0078 — the account's own mailbox, named by its mailbox key;
        // every row is opened with that key. The owner seals the co-publisher
        // Welcome as a `welcomeRendezvous` row; a plain `welcome` row is tried
        // too (whichever opens this channel's PublisherGroup counts).
        // The mailbox is the ACCOUNT's (ADR-0090: `identity` is the device).
        let account = try ChannelPublisherIdentity.ownerAccountIdentity()
        let mailboxKey = MailboxKey.fromAccountIdentity(accountIdentity: account)
        let signer = dsMailboxSigner(accountIdentity: account)
        let mailbox = signer.mailboxAddress()
        var candidates: [Data] = []
        var after: Int64 = 0
        for _ in 0..<Self.maxMailboxPages {
            let page = try await ds.fetchMailbox(
                pseudonym: mailbox, after: after, waitSecs: 0, auth: DsAuth(signer: signer))
            for row in page.messages {
                guard let opened = MailboxRows.open(row.payload, key: mailboxKey) else { continue }
                switch opened.kind {
                case .welcomeRendezvous, .welcome: candidates.append(opened.body)
                case .contactRequest, .contactAck, .linkRefusal: continue
                }
            }
            guard let last = page.messages.last, page.hasMore, last.id > after else { break }
            after = last.id
        }
        // Join the PublisherGroup from the Welcome — this persists the joined
        // MLS state under the PublisherGroup `group_id` the link conveyed in
        // the identity's provider. The handle itself is throwaway (ARC frees
        // it); `freshPublisherKeys` re-opens the now-joined PublisherGroup
        // under that id and derives the shared content-key seed + a fresh
        // per-device signing key so this device can publish. Newest first,
        // and a Welcome sealed to a KeyPackage this identity does not hold is
        // skipped rather than ending the wait.
        // ADR-0081 amendment — a PublisherGroup carries no role table, so it
        // joins through its own entry point: `Group.joinFromWelcome` refuses
        // its Welcome, and this one refuses a Welcome for any other group (or
        // a chat's) before anything is written.
        guard Self.hexToData(channelIdHex) != nil else {
            throw CoPublisherError.badChannelId(channelIdHex)
        }
        for welcome in candidates.reversed() {
            // Only this channel's PublisherGroup counts: `identity` may hold
            // the keys of other requests.
            if let keys = try joinCoPublishWelcome(
                identity: identity, accountIdB64: accountIdB64, welcome: welcome, groupId: groupId,
                channelIdHex: channelIdHex, ownerAccountKey: ownerAccountKey,
                existingSigningSeed: existingSigningSeed) {
                return keys
            }
        }
        return nil
    }

    /// Join the PublisherGroup `groupId` of channel `channelIdHex` from one
    /// Welcome and become a publisher: the keys at the post-approve epoch, or
    /// `nil` when the Welcome is not this group's or not sealed to a
    /// KeyPackage `identity` holds (nothing is written then). Used by the
    /// Accept wait and by the mailbox receive loop
    /// (``CoPublisherJoin/landPendingWelcome(_:accountIdB64:identity:state:)``).
    public static func joinCoPublishWelcome(
        identity: Identity,
        accountIdB64: String?,
        welcome: Data,
        groupId: Data,
        channelIdHex: String,
        ownerAccountKey: Data,
        existingSigningSeed: Data? = nil
    ) throws -> ChannelPublisherKeys? {
        guard let channelId = Self.hexToData(channelIdHex) else {
            throw CoPublisherError.badChannelId(channelIdHex)
        }
        // Security pass 2026-11-15 (F2) — a Welcome this device already
        // joined for the channel is an old one served again: not joined.
        let replay = accountIdB64.map { ChannelDeviceAdmissionStore(accountIdB64: $0) }
        let welcomeHash = ChannelDeviceAdmission.welcomeReplayHash(welcome)
        if replay?.hasJoinedWelcome(welcomeHash, channelId: channelIdHex) == true { return nil }
        // ADR-0090 — refused unless the Welcome's sender is a device of the
        // owner account the capability named.
        guard (try? publisherGroupJoinFromWelcome(
            identity: identity, welcome: welcome, groupId: groupId, channelId: channelId,
            ownerAccountKey: ownerAccountKey)) != nil
        else { return nil }
        replay?.recordJoinedWelcome(welcomeHash, channelId: channelIdHex)
        // ADR-0057 — a co-publisher never claims the journal (idempotent with
        // the mark set when the request was sent).
        CoPublishedJournals.shared.mark(
            journalIdHex: DsClient.pseudonymHex(groupId), accountIdB64: accountIdB64)
        let keys = try ChannelsComposer.freshPublisherKeys(
            identity: identity,
            accountIdB64: accountIdB64,
            channelIdHex: channelIdHex,
            existingSigningSeed: existingSigningSeed,
            joinedGroupId: groupId)
        // ADR-0059 section 9: this account publishes here as a co-publisher,
        // so its writes are signed with the co-publisher key from the first one.
        if let account = try? ChannelPublisherIdentity.ownerAccountIdentity() {
            try? ChannelsClient.rememberPublisherRole(
                channelIdHex: channelIdHex, accountIdentity: account, role: .copublisher)
        }
        return keys
    }

    /// Bound on the mailbox pages one Welcome look-up reads.
    static let maxMailboxPages = 50

    // MARK: - helpers

    static func hexToData(_ hex: String) -> Data? {
        guard hex.count % 2 == 0, !hex.isEmpty else { return nil }
        var out = Data(capacity: hex.count / 2)
        var idx = hex.startIndex
        while idx < hex.endIndex {
            let next = hex.index(idx, offsetBy: 2)
            guard let b = UInt8(hex[idx..<next], radix: 16) else { return nil }
            out.append(b)
            idx = next
        }
        return out
    }

    static func base64UrlEncode(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    static func base64UrlDecode(_ slug: String) -> Data? {
        var s = slug
            .replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        let remainder = s.count % 4
        if remainder != 0 {
            s.append(String(repeating: "=", count: 4 - remainder))
        }
        return Data(base64Encoded: s)
    }
}

/// Errors surfaced by the co-publisher owner flow.
public enum CoPublisherError: Error, LocalizedError {
    case badChannelId(String)
    case unexpectedServerEpoch(UInt32)
    case malformedSubscriberHex(String)
    /// Not a `/g/` ledger link, or one whose row is gone (revoked, expired).
    case notACoPublisherLink
    case notAChannelGroup
    /// The channel's PublisherGroup is not in this device's channel
    /// provider: this device reads the channel and publishes nothing in it
    /// (`docs/ux/second-device.md` section 4a).
    case notOnThisDevice
    /// ADR-0028 amendment 2026-09-28 — the link's secret is missing or the
    /// capability does not open under it.
    case linkDamaged
    /// ADR-0090 — another owner device's commit took the epoch first: this
    /// one was abandoned; the next pass reads the journal and tries again.
    case commitLost

    public var errorDescription: String? {
        switch self {
        case .commitLost:
            return "another device's commit took the epoch first"
        case .badChannelId(let hex):
            return "channel id hex is not valid: \(hex)"
        case .unexpectedServerEpoch(let epoch):
            return "unexpected server epoch \(epoch) — cannot reseal"
        case .malformedSubscriberHex(let hex):
            return "subscriber pubkey hex is not valid: \(hex)"
        case .notACoPublisherLink:
            return "not a live /g/ co-publisher link"
        case .notAChannelGroup:
            return "co-publisher link does not target a channel PublisherGroup"
        case .notOnThisDevice:
            return "the channel's publisher group is not on this device"
        case .linkDamaged:
            return inviteLinkDamagedMessage
        }
    }
}

/// Co-publisher keys read off PublisherGroup knock queues (ADR-0059
/// section 1), filed under the joiner's account key each binding was
/// verified under, so the approve looks one up by the joiner key it approves.
///
/// Durable per account (2026-11-04): the knock cursor moves past the row, so
/// a binding kept in memory only was gone after a restart and a joiner
/// approved after one was never appointed. Public keys only (the joiner's
/// account key and the co-publisher key it asked for), so plain
/// `UserDefaults` under ``AccountStoreGate``; removed once the appointment
/// landed or the request was rejected, and with the account
/// (``AccountLocalDataWipe``).
actor CoPublisherKeyBook {
    static let shared = CoPublisherKeyBook()

    static func storeKey(_ accountIdB64: String) -> String { "lakko.copublisher-keys.v1.\(accountIdB64)" }

    private static func slot(groupId: Data, joinerKey: Data) -> String {
        (groupId + joinerKey).base64EncodedString()
    }

    private nonisolated func load(_ accountIdB64: String) -> [String: String] {
        UserDefaults.standard.dictionary(forKey: Self.storeKey(accountIdB64)) as? [String: String] ?? [:]
    }

    private nonisolated func save(_ all: [String: String], _ accountIdB64: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        if all.isEmpty {
            UserDefaults.standard.removeObject(forKey: Self.storeKey(accountIdB64))
        } else {
            UserDefaults.standard.set(all, forKey: Self.storeKey(accountIdB64))
        }
    }

    func record(
        groupId: Data, binding: FfiChannelCoPublisherBinding,
        accountIdB64: String? = AcceptedContactsStore.boundAccountIdB64
    ) {
        guard let accountIdB64 else { return }
        var all = load(accountIdB64)
        all[Self.slot(groupId: groupId, joinerKey: binding.accountPubkey)] =
            binding.copublisherPubkey.base64EncodedString()
        save(all, accountIdB64)
    }

    func key(
        groupId: Data, joinerKey: Data, accountIdB64: String? = AcceptedContactsStore.boundAccountIdB64
    ) -> Data? {
        guard let accountIdB64 else { return nil }
        return load(accountIdB64)[Self.slot(groupId: groupId, joinerKey: joinerKey)]
            .flatMap { Data(base64Encoded: $0) }
    }

    /// The appointment landed, or the request was rejected.
    func forget(
        groupId: Data, joinerKey: Data, accountIdB64: String? = AcceptedContactsStore.boundAccountIdB64
    ) {
        guard let accountIdB64 else { return }
        var all = load(accountIdB64)
        guard all.removeValue(forKey: Self.slot(groupId: groupId, joinerKey: joinerKey)) != nil else { return }
        save(all, accountIdB64)
    }

    nonisolated static func clear(accountIdB64: String) {
        UserDefaults.standard.removeObject(forKey: storeKey(accountIdB64))
        UserDefaults.standard.removeObject(forKey: publishersKey(accountIdB64))
        UserDefaults.standard.removeObject(forKey: appointedKey(accountIdB64))
        UserDefaults.standard.removeObject(forKey: bindingSentKey(accountIdB64))
    }

    // MARK: - Who else publishes (ADR-0087 section 15)

    /// Posted when the publisher record of a channel changed.
    static let publishersChanged = Notification.Name("me.lakko.channels.publishers-changed")

    nonisolated static func publishersKey(_ accountIdB64: String) -> String {
        "lakko.copublisher-publishers.v1.\(accountIdB64)"
    }

    /// The co-publishers this account approved, by channel, read
    /// synchronously so the info screen can answer while it draws. A
    /// co-publisher's own device always has another publisher, the owner.
    nonisolated static func publishers(
        channelIdHex: String, accountIdB64: String? = AcceptedContactsStore.boundAccountIdB64
    ) -> Set<String> {
        guard let accountIdB64,
              let all = UserDefaults.standard.dictionary(forKey: publishersKey(accountIdB64)) as? [String: [String]]
        else { return [] }
        return Set(all[channelIdHex.lowercased()] ?? [])
    }

    /// Whether anyone but this account publishes the channel.
    nonisolated static func hasOtherPublishers(
        channelIdHex: String, isOwner: Bool,
        accountIdB64: String? = AcceptedContactsStore.boundAccountIdB64
    ) -> Bool {
        !isOwner || !publishers(channelIdHex: channelIdHex, accountIdB64: accountIdB64).isEmpty
    }

    /// Replace the channel's publisher set (the joiner keys, hex) and say so.
    nonisolated static func setPublishers(
        _ keysHex: Set<String>, channelIdHex: String,
        accountIdB64: String? = AcceptedContactsStore.boundAccountIdB64
    ) {
        guard let accountIdB64, let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var all = UserDefaults.standard.dictionary(forKey: publishersKey(accountIdB64)) as? [String: [String]] ?? [:]
        let id = channelIdHex.lowercased()
        guard Set(all[id] ?? []) != keysHex else { return }
        all[id] = keysHex.isEmpty ? nil : keysHex.sorted()
        UserDefaults.standard.set(all, forKey: publishersKey(accountIdB64))
        NotificationCenter.default.post(name: publishersChanged, object: id)
    }

    nonisolated static func appointedKey(_ accountIdB64: String) -> String {
        "lakko.copublisher-appointed.v1.\(accountIdB64)"
    }

    /// The co-publisher key appointed for `accountKey` in a channel, kept
    /// past the appointment so a removal can revoke it.
    nonisolated static func appointedKey(
        channelIdHex: String, accountKey: Data, accountIdB64: String? = AcceptedContactsStore.boundAccountIdB64
    ) -> Data? {
        guard let accountIdB64,
              let all = UserDefaults.standard.dictionary(forKey: appointedKey(accountIdB64)) as? [String: String]
        else { return nil }
        return all["\(channelIdHex.lowercased()):\(accountKey.base64EncodedString())"]
            .flatMap { Data(base64Encoded: $0) }
    }

    /// Record (or, with `nil`, drop) the appointment of `accountKey`.
    nonisolated static func noteAppointed(
        channelIdHex: String, accountKey: Data, copublisherKey: Data?,
        accountIdB64: String? = AcceptedContactsStore.boundAccountIdB64
    ) {
        guard let accountIdB64, let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var all = UserDefaults.standard.dictionary(forKey: appointedKey(accountIdB64)) as? [String: String] ?? [:]
        all["\(channelIdHex.lowercased()):\(accountKey.base64EncodedString())"] = copublisherKey?.base64EncodedString()
        UserDefaults.standard.set(all, forKey: appointedKey(accountIdB64))
    }

    // MARK: - The publisher's own binding in the group (ADR-0087 section 15.2)

    nonisolated static func bindingSentKey(_ accountIdB64: String) -> String {
        "lakko.copublisher-binding-sent.v1.\(accountIdB64)"
    }

    /// The PublisherGroup epoch this device last sent its own co-publisher
    /// binding at, by channel.
    nonisolated static func bindingSentEpoch(
        channelIdHex: String, accountIdB64: String? = AcceptedContactsStore.boundAccountIdB64
    ) -> UInt64? {
        guard let accountIdB64,
              let all = UserDefaults.standard.dictionary(forKey: bindingSentKey(accountIdB64)) as? [String: String]
        else { return nil }
        return all[channelIdHex.lowercased()].flatMap(UInt64.init)
    }

    nonisolated static func noteBindingSent(
        channelIdHex: String, epoch: UInt64, accountIdB64: String? = AcceptedContactsStore.boundAccountIdB64
    ) {
        guard let accountIdB64, let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var all = UserDefaults.standard.dictionary(forKey: bindingSentKey(accountIdB64)) as? [String: String] ?? [:]
        all[channelIdHex.lowercased()] = String(epoch)
        UserDefaults.standard.set(all, forKey: bindingSentKey(accountIdB64))
    }

    /// The owner side: a binding a publisher sent in the group, filed as the
    /// appointed key of the account it was sent from. A binding is kept only
    /// when its signature verifies, it names this channel, and its account is
    /// the MLS sender's: the co-publisher key is derived from that account's
    /// seed and the channel, so it is the key the approving device appointed.
    nonisolated static func noteBindings(
        _ applications: [CoPublisher.JournalApplication], ownerAccountKey: Data?, channelIdHex: String,
        accountIdB64: String? = AcceptedContactsStore.boundAccountIdB64
    ) {
        guard let channelId = ChannelsClient.hexToData(channelIdHex) else { return }
        for app in applications {
            guard let sender = app.senderAccount, sender != ownerAccountKey,
                  let binding = try? decodeChannelCopublisherBinding(bytes: app.data),
                  binding.channelId == channelId, binding.accountPubkey == sender,
                  appointedKey(channelIdHex: channelIdHex, accountKey: sender, accountIdB64: accountIdB64)
                    != binding.copublisherPubkey
            else { continue }
            noteAppointed(
                channelIdHex: channelIdHex, accountKey: sender, copublisherKey: binding.copublisherPubkey,
                accountIdB64: accountIdB64)
        }
    }

    /// An approve landed: the joiner publishes.
    nonisolated static func notePublisher(channelIdHex: String, joinerKey: Data) {
        var keys = publishers(channelIdHex: channelIdHex)
        keys.insert(joinerKey.map { String(format: "%02x", $0) }.joined())
        setPublishers(keys, channelIdHex: channelIdHex)
    }
}

/// ADR-0057 — PublisherGroup journals an ACCOUNT on this device joined as a
/// CO-publisher. Only the channel owner claims a PublisherGroup journal: a
/// co-publisher's root is derived from its own account seed, so its claim
/// would squat the journal against the owner. A PublisherGroup exposes no role
/// table, so the device remembers that the account asked to co-publish. Plain
/// `UserDefaults`: a journal id is a public DS address.
///
/// Per account (2026-09-30): "co-publisher" is a fact about the account, not
/// the phone — the owner of a channel signed in on a phone where another
/// account co-publishes it must still claim. The set is filed under the
/// account (`me.lakko.ds.co-published.v2.<account>`). A read resolves the
/// account at each call (by default the one ``AcceptedContactsStore`` has
/// bound). A mark names the account its work was started for and is made only
/// while that account is still the bound one: a request started by account A
/// and finishing after B signed in must not file A's journal under B (B would
/// then never claim it, were B the channel's owner). No account: nothing is
/// marked and nothing reads as marked. Logout keeps it; forget-device drops it
/// through ``clear(accountIdB64:)``.
public final class CoPublishedJournals {
    public static let shared = CoPublishedJournals()

    private let defaults: UserDefaults
    private let accountIdB64: () -> String?
    private static let prefix = "me.lakko.ds.co-published.v2."
    /// The device-wide key this set used before it was per account. Never
    /// read; removed by ``clear(accountIdB64:)``.
    static let legacyKey = "me.lakko.ds.co-published.v1"

    init(
        defaults: UserDefaults = .standard,
        accountIdB64: @escaping () -> String? = { AcceptedContactsStore.boundAccountIdB64 }
    ) {
        self.defaults = defaults
        self.accountIdB64 = accountIdB64
    }

    private func key() -> String? {
        accountIdB64().map { Self.prefix + $0 }
    }

    /// Mark `journalIdHex` for `accountIdB64`, the account the caller's work
    /// was started for. Best effort: no account named, one that is no longer
    /// the bound one, or one whose session has ended (``AccountStoreGate``),
    /// marks nothing.
    public func mark(journalIdHex: String, accountIdB64 captured: String?) {
        try? markOrEnd(journalIdHex: journalIdHex, accountIdB64: captured)
    }

    /// ``mark(journalIdHex:accountIdB64:)`` for a caller about to ask to
    /// co-publish: the mark is what stops this account claiming the journal
    /// later, so the request is not sent without it. Throws
    /// ``StaleBindingError`` with no account named or when `captured` is not
    /// the bound account any more, and ``AccountStoreGate/Sealed`` once the
    /// account's session has ended; nothing is written then.
    public func markOrEnd(journalIdHex: String, accountIdB64 captured: String?) throws {
        guard let account = captured, account == accountIdB64() else { throw StaleBindingError() }
        let gate = try AccountStoreGate.enterOrEnd(account)
        defer { gate.leave() }
        let key = Self.prefix + account
        var set = Set(defaults.stringArray(forKey: key) ?? [])
        guard set.insert(journalIdHex.lowercased()).inserted else { return }
        defaults.set(Array(set).sorted(), forKey: key)
    }

    public func contains(journalIdHex: String) -> Bool {
        guard let key = key() else { return false }
        return (defaults.stringArray(forKey: key) ?? []).contains(journalIdHex.lowercased())
    }

    /// The channel -> PublisherGroup id map of the co-published channels
    /// (ADR-0078 section 6), `me.lakko.ds.co-published-channels.v1.<account>`.
    private static let channelsPrefix = "me.lakko.ds.co-published-channels.v1."

    /// ADR-0078 section 6 — ``markOrEnd(journalIdHex:accountIdB64:)`` that
    /// also remembers which channel the journal is. The PublisherGroup id is
    /// the OWNER's derivation, which a co-publisher cannot compute, so this
    /// is how it opens the group it joined (``journalIdHex(channelIdHex:)``).
    public func markOrEnd(journalIdHex: String, channelIdHex: String, accountIdB64 captured: String?) throws {
        try markOrEnd(journalIdHex: journalIdHex, accountIdB64: captured)
        guard let account = captured else { throw StaleBindingError() }
        let gate = try AccountStoreGate.enterOrEnd(account)
        defer { gate.leave() }
        let key = Self.channelsPrefix + account
        var map = (defaults.dictionary(forKey: key) as? [String: String]) ?? [:]
        map[channelIdHex.lowercased()] = journalIdHex.lowercased()
        defaults.set(map, forKey: key)
    }

    /// The PublisherGroup id hex of `channelIdHex` when the bound account
    /// co-publishes it, else `nil` (it owns the channel, or does not publish).
    public func journalIdHex(channelIdHex: String) -> String? {
        guard let account = accountIdB64() else { return nil }
        let map = defaults.dictionary(forKey: Self.channelsPrefix + account) as? [String: String]
        return map?[channelIdHex.lowercased()]
    }

    /// Forget-device: drop one account's set (and the old device-wide row).
    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.prefix + accountIdB64)
        defaults.removeObject(forKey: Self.channelsPrefix + accountIdB64)
        defaults.removeObject(forKey: Self.legacyKey)
    }
}
