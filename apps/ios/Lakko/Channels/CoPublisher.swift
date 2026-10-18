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

    /// Open (or create) the persistent PublisherGroup handle for a channel.
    ///
    /// `accountIdB64` is the account whose `identity` this is, captured when
    /// the caller's work began: a group founded here is marked for it
    /// (``markFounded(_:accountIdB64:)``).
    public static func openPublisherGroup(
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
            let created = try publisherGroupOpenOrCreate(identity: identity, channelId: channelId)
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
        return try publisherGroupLoad(identity: identity, channelId: channelId)
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

    /// Mint a co-publisher invite link over the PublisherGroup's MLS `group_id`.
    /// A non-member opens it, requests to join, and the owner approves (reusing
    /// the ADR-0027/0028 external-join machinery). `0` = never expires.
    ///
    /// ADR-0028 amendment 2026-09-28 — the capability is sealed under a 12-byte
    /// link secret that rides the fragment. A co-publisher link was never put
    /// on the discovery ledger, so it keeps the inline shape:
    /// `<origin>/g/<b64url sealed>#<secret>`.
    ///
    /// ADR-0078 section 6 — the joiner cannot recover the channel id from the
    /// group id any more, so the capability's sealed preview carries it (and
    /// the channel's name when known).
    public static func createCoPublisherLink(
        identity: Identity,
        publisherGroup: PublisherGroup,
        channelName: String? = nil,
        origin: String = defaultOrigin
    ) throws -> String {
        let groupId = try publisherGroup.groupId()
        let mint = try composeCoPublisherInviteCapability(
            issuerSigningSeed: identity.ed25519Seed(),
            groupId: groupId,
            expiryMs: 0,
            channelId: try publisherGroup.channelId(),
            channelName: channelName
        )
        return InviteLinkSecret.link(
            base: "\(origin)\(urlPrefix)\(Self.base64UrlEncode(mint.sealed))",
            secret: mint.linkSecret)
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
        try dsJournalRootSigner(accountIdentity: identity, journalId: try publisherGroup.groupId())
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
    }

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
        readKnocks: Bool = true
    ) async throws -> PollOutcome {
        let groupId = try publisherGroup.groupId()
        let raw = try await ds.fetchGroup(
            groupId: groupId, after: 0, waitSecs: 0,
            auth: auth(ds: ds, identity: identity, publisherGroup: publisherGroup))
        for m in raw.messages {
            do {
                _ = try publisherGroup.processIncomingVerifyingRoles(
                    identity: identity, bytes: m.payload)
            } catch {
                // A commit we have already merged (epoch mismatch) — skip; the
                // FIFO journal catches up on any genuinely-new commit on the next
                // poll.
            }
        }
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
            knockCursor: knockCursor)
    }

    /// The pending co-publisher external-join requests — each a 32-byte joiner
    /// leaf signature key (the `proposalRef` for approve / reject).
    public static func pending(publisherGroup: PublisherGroup) throws -> [Data] {
        try publisherGroup.pendingExternalJoins()
    }

    /// Approve a pending co-publisher. Commits the Add (advances the
    /// PublisherGroup epoch), then, in order: registers the new epoch's
    /// access key and publishes the commit to the journal in one request
    /// (ADR-0081),
    /// sends the Welcome to the joiner's own mailbox (point 8: a non-member
    /// cannot read the claimed journal), RESEALS the new-epoch channel seed
    /// to all subscribers (the Add changed the exporter output, so existing
    /// subscribers must get the new seed or they break), and appoints the
    /// newcomer's co-publisher key (ADR-0059).
    ///
    /// G-50 — the server epoch the reseal binds to is read BEFORE the Add,
    /// because after the eager merge nothing may fail before the commit's
    /// record is durable. The record (``PublisherCommitEntry``) is written
    /// with the N+1 provider snapshot, fail-closed, before any step runs; a
    /// failed step leaves the record with what landed, and the next session
    /// resumes it.
    @MainActor
    public static func approve(
        context: ChannelPublisherCommits.Context,
        state: ChannelsUiState?,
        proposalRef: Data
    ) async throws {
        let current = try await context.channelsClient.fetchEpoch(channelIdHex: context.channelIdHex)
        let serverTarget = current &+ 1
        guard serverTarget > current else { throw CoPublisherError.unexpectedServerEpoch(current) }
        let (identity, group) = try context.open()
        // ADR-0081 — the proof of the epoch the Add leaves, taken before the
        // eager merge and recorded with the approve: the commit route is
        // proven under it, live and on a replay after a restart.
        let proofEpoch = try group.epoch()
        let proofSeed = try group.dsAccessSeed(identity: identity)
        let added = try group.approveExternalJoin(identity: identity, proposalRef: proposalRef)
        let entry = PublisherCommitEntry(
            kind: .approve,
            channelIdHex: context.channelIdHex.lowercased(),
            groupIdHex: DsClient.pseudonymHex(try group.groupId()),
            targetEpoch: try group.epoch(),
            serverTargetEpoch: serverTarget,
            commitB64: added.commit.base64EncodedString(),
            welcomeB64: added.welcome.base64EncodedString(),
            joinerKeyB64: proposalRef.base64EncodedString(),
            proofEpoch: proofEpoch,
            proofSeedB64: proofSeed.base64EncodedString())
        try ChannelPublisherCommits.record(entry, accountIdB64: context.accountIdB64)
        try await PublisherCommitRunner.run(
            entry, ports: ChannelPublisherCommits.ports(context, state: state))
        ChannelPublisherCommits.refreshContentSeed(context, state: state)
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
        let resolve: () async throws -> MailboxWriteAttempt = {
            let plan = await MainActor.run {
                ReachCenter.shared.writePlan(toAccountKey: joinerKey)
            }
            guard let plan else {
                NSLog("ADR-0078: no verified mailbox for the co-publisher; its Welcome was not written")
                throw MailboxUnknown()
            }
            return MailboxWriteAttempt(target: plan.target, auth: plan.auth)
        }
        func publish(_ attempt: MailboxWriteAttempt) async throws {
            _ = try await ds.publishMailbox(
                pseudonym: attempt.target.address,
                payload: try attempt.target.seal(kind: .welcomeRendezvous, body: welcome),
                writeAuth: attempt.auth)
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
        /// This device's leaf signature key — names the mailbox the owner sends
        /// the Welcome to after approving (ADR-0053).
        public let joinerKey: Data
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
        persistProposalKeys: @escaping @MainActor () throws -> Void = {
            try ChannelPublisherIdentity.shared.persistOrThrow()
        }
    ) async throws -> CoPublishRequest {
        // ADR-0028 amendment 2026-09-28 — `<origin>/g/<b64url sealed>#<secret>`:
        // the capability is sealed under the fragment's secret.
        let parts = InviteLinkSecret.split(link)
        guard let idx = parts.base.range(of: urlPrefix, options: .backwards) else {
            throw CoPublisherError.notACoPublisherLink(parts.base)
        }
        let slug = String(parts.base[idx.upperBound...])
        guard let sealed = Self.base64UrlDecode(slug) else {
            throw CoPublisherError.notACoPublisherLink(parts.base)
        }
        guard let secret = InviteLinkSecret.read(parts.fragment) else {
            throw CoPublisherError.linkDamaged
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
        let binding = try composeChannelCopublisherBinding(
            accountIdentity: identity, channelId: channelIdBytes)
        // ADR-0057 — this device asks to CO-publish, so it is not the channel
        // owner: it must never claim the PublisherGroup journal. Recorded
        // before the knock, fail closed: an unrecorded request could later
        // let this account's claim squat the owner's journal.
        // ADR-0078 section 6: the channel -> group id pair is recorded with
        // it, since this account cannot derive the owner's group id.
        try CoPublishedJournals.shared.markOrEnd(
            journalIdHex: DsClient.pseudonymHex(groupId), channelIdHex: channelIdHex,
            accountIdB64: accountIdB64)
        // ADR-0053 point 6 — the knock: a probe learns the registered epoch,
        // the proposal is built for it, a second knock carries the self-signed
        // attribution (OQ2, advisory) and the proposal. No journal read.
        // ADR-0060 section 6 — the `reply` grant the owner writes the Welcome
        // with, registered before the knock.
        let replyGrant = try await ReachCenter.shared.newReplyGrant(
            purpose: ReplyPurpose.groupJoin(groupId))
        try await DsKnockJoin(
            knock: { payloads in
                // ADR-0059 section 1 — this account's co-publisher key for the
                // channel, signed by its account identity key (the key the
                // owner approves), rides beside the proposal. Not on the probe.
                let all = payloads.isEmpty ? payloads : payloads + [binding]
                return try await ds.knock(groupId: groupId, payloads: all).epoch
            },
            createProposal: { epoch in
                try identity.createExternalJoinProposal(groupId: groupId, epoch: epoch)
            },
            attribution: {
                try? composeGroupJoinAttribution(
                    joinerSigningSeed: identity.ed25519Seed(),
                    capabilityBytes: wire,
                    replyGrant: replyGrant)
            },
            // ADR-0077 decision 5 — the proposal's KeyPackage private keys,
            // durable BEFORE the proposal is published. A request whose keys
            // are not on disk could be approved into a Welcome nobody can
            // open once the app is closed. Refused (another account bound,
            // the account's stores sealed or wiped) or failed: nothing is
            // published.
            persistBeforeKnock: {
                guard let account = accountIdB64,
                      account == AcceptedContactsStore.boundAccountIdB64 else {
                    throw StaleBindingError()
                }
                try await persistProposalKeys()
            }
        ).run()
        return CoPublishRequest(
            groupId: groupId,
            channelIdHex: channelIdHex,
            joinerKey: identity.signaturePublicKey()
        )
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
        // G-29 — reuse the persisted per-channel signing seed if this device
        // has co-published to the channel before (nil on a first join).
        existingSigningSeed: Data? = nil
    ) async throws -> ChannelPublisherKeys? {
        // ADR-0078 — the account's own mailbox, named by its mailbox key;
        // every row is opened with that key. The owner seals the co-publisher
        // Welcome as a `welcomeRendezvous` row; a plain `welcome` row is tried
        // too (whichever opens this channel's PublisherGroup counts).
        let mailboxKey = MailboxKey.fromAccountIdentity(accountIdentity: identity)
        let signer = dsMailboxSigner(accountIdentity: identity)
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
                case .contactRequest, .contactAck: continue
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
        var joined = false
        for welcome in candidates.reversed() {
            // Only this channel's PublisherGroup counts: `identity` may hold
            // the keys of other requests.
            if let group = try? Group.joinFromWelcome(welcome: welcome, identity: identity),
               (try? group.groupId()) == groupId {
                joined = true
                break
            }
        }
        guard joined else { return nil }
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
        try? ChannelsClient.rememberPublisherRole(
            channelIdHex: channelIdHex, accountIdentity: identity, role: .copublisher)
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
    case notACoPublisherLink(String)
    case notAChannelGroup
    /// ADR-0028 amendment 2026-09-28 — the link's secret is missing or the
    /// capability does not open under it.
    case linkDamaged

    public var errorDescription: String? {
        switch self {
        case .badChannelId(let hex):
            return "channel id hex is not valid: \(hex)"
        case .unexpectedServerEpoch(let epoch):
            return "unexpected server epoch \(epoch) — cannot reseal"
        case .malformedSubscriberHex(let hex):
            return "subscriber pubkey hex is not valid: \(hex)"
        case .notACoPublisherLink(let link):
            return "not a /g/ co-publisher link: \(link)"
        case .notAChannelGroup:
            return "co-publisher link does not target a channel PublisherGroup"
        case .linkDamaged:
            return inviteLinkDamagedMessage
        }
    }
}

/// Co-publisher keys read off PublisherGroup knock queues (ADR-0059
/// section 1), filed under the joiner's account key each binding was
/// verified under, so the approve looks one up by the joiner key it approves.
/// In memory: a binding read before a restart is not kept (the knock cursor
/// has moved past it), so a joiner approved after a restart is not appointed.
actor CoPublisherKeyBook {
    static let shared = CoPublisherKeyBook()
    private var keys: [Data: Data] = [:]

    private static func slot(groupId: Data, joinerKey: Data) -> Data {
        groupId + joinerKey
    }

    func record(groupId: Data, binding: FfiChannelCoPublisherBinding) {
        keys[Self.slot(groupId: groupId, joinerKey: binding.accountPubkey)] =
            binding.copublisherPubkey
    }

    func key(groupId: Data, joinerKey: Data) -> Data? {
        keys[Self.slot(groupId: groupId, joinerKey: joinerKey)]
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
