// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0101 section 7 — the channel runtime of a key reset: the recovery key
// every owned channel registers, the returns a peer sends a reset person,
// and what the reset device does with the returns it read. The rules are in
// ``ChannelReturnRules``; this is the I/O around them, driven from the
// publisher pass (``ChannelPublisherLoops``). iOS mirror of
// `apps/web/src/channels/channel-return-runtime.ts`.

@MainActor
enum ChannelReturnRuntime {
    /// How many card pages a channel is read for returns (chosen).
    static let returnCardPages = 10
    /// How long a publisher Accept is watched before the return is counted
    /// handled (chosen).
    static let acceptSettleSeconds: UInt64 = 20

    /// What the last pass ran with, so a held return is acted on at once
    /// (``wake()``).
    private static var last: (channels: ChannelsUiState, client: ChannelsClient, seed: Data, accountIdB64: String)?
    private static var running = false

    /// One pass, after the publisher pass: register owed recovery keys, act
    /// on held returns, send the returns owed, settle recovered channels.
    static func pass(channels: ChannelsUiState, client: ChannelsClient, seed: Data, accountIdB64: String) async {
        last = (channels, client, seed, accountIdB64)
        guard !running else { return }
        running = true
        defer { running = false }
        await registerOwedRecoveryKeys(channels: channels, client: client, seed: seed, accountIdB64: accountIdB64)
        await runHeldReturns(channels: channels, client: client, seed: seed, accountIdB64: accountIdB64)
        await sendReturns(channels: channels, client: client, seed: seed, accountIdB64: accountIdB64)
        settleRecovered(channels: channels, seed: seed, accountIdB64: accountIdB64)
    }

    /// The mailbox kept a channel return: act on it now.
    static func wake() {
        guard let last, !running else { return }
        Task { @MainActor in
            guard !running, AcceptedContactsStore.boundAccountIdB64 == last.accountIdB64 else { return }
            running = true
            defer { running = false }
            await runHeldReturns(
                channels: last.channels, client: last.client, seed: last.seed, accountIdB64: last.accountIdB64)
        }
    }

    static func reset() {
        last = nil
    }

    // MARK: - The channel recovery key

    struct RecoveryDisabled: Error {}

    /// The channel's recovery key: its recovery input blinded here,
    /// evaluated by auth under this account's per-account key (auth never
    /// learns the channel id), finalised here. Throws ``RecoveryDisabled`` on
    /// `503`.
    static func deriveRecoveryKey(idHex: String) async throws -> ChannelRecoveryKey {
        guard let token = AmbientSessionToken.shared.current, let channelId = ChannelsClient.hexToData(idHex) else {
            throw ChannelsApiError(status: -1, code: "no-session", detail: "no session for the recovery key")
        }
        let blinder = try OprfBlinder.blindChannelRecovery(channelId: channelId)
        guard let blinded = Data(base64Encoded: blinder.blindedB64()) else {
            throw ChannelsApiError(status: -1, code: "bad-blind", detail: "blinded element not base64")
        }
        let evaluation: Data
        do {
            evaluation = try await AuthClient().channelRecoveryEvaluate(sessionToken: token, blindedElement: blinded)
        } catch AuthClientError.http(let status, _, _) where status == 503 {
            throw RecoveryDisabled()
        }
        var output = try blinder.finalize(evaluatedB64: evaluation.base64EncodedString()).output
        defer { output.resetBytes(in: 0..<output.count) }
        return ChannelRecoveryKey.fromOprfOutput(oprfOutput: output)
    }

    /// Register the recovery key of every channel this device owns that has
    /// none recorded (a new channel is picked up at the first pass after its
    /// creation). A `503` stops the pass quietly; any other failure leaves
    /// the channel owing.
    static func registerOwedRecoveryKeys(
        channels: ChannelsUiState, client: ChannelsClient, seed: Data, accountIdB64: String
    ) async {
        let store = ChannelRecoveryRegisteredStore()
        let owned = channels.keysByChannel.keys.filter {
            ChannelPublisherLoops.rotates(channelId: $0, channels: channels)
        }
        let owing = ChannelReturnRules.owingRecoveryKey(owned: owned, registered: store.read(accountIdB64: accountIdB64))
        guard !owing.isEmpty, let owner = try? Identity.fromSeed(seed: seed) else { return }
        for idHex in owing {
            let key: ChannelRecoveryKey
            do {
                key = try await deriveRecoveryKey(idHex: idHex)
            } catch is RecoveryDisabled {
                return
            } catch {
                continue
            }
            if await client.putRecoveryKey(channelIdHex: idHex, ownerIdentity: owner, recoveryPk: key.verifyingKey()) == 204 {
                store.mark(idHex, accountIdB64: accountIdB64)
            }
        }
    }

    // MARK: - The peer side: returns to send

    struct OpenedCard {
        let card: ReturnCard
        let bindingSig: Data
        let profileHash: Data
        let subscriberPk: Data
    }

    static func openCards(channelId: String, channels: ChannelsUiState) async throws -> [OpenedCard] {
        guard let idBytes = ChannelsClient.hexToData(channelId) else { return [] }
        let seeds = channels.readSeeds(channelId: channelId)
        var out: [OpenedCard] = []
        var after: String?
        for _ in 0..<returnCardPages {
            let page = try await ChannelsClient().listMemberCards(channelIdHex: channelId, after: after)
            for member in page.members {
                guard let epoch = try? memberCardEpoch(card: member.card), let seed = seeds[epoch],
                      let opened = try? openMemberCard(
                          seed: seed, card: member.card, channelId: idBytes, subscriberPk: member.subscriberPk)
                else { continue }
                out.append(OpenedCard(
                    card: ReturnCard(
                        identityPkB64: base64Encode(opened.identityPk),
                        subscriberPkB64: base64Encode(member.subscriberPk)),
                    bindingSig: opened.bindingSig, profileHash: opened.profileHash,
                    subscriberPk: member.subscriberPk))
            }
            after = page.nextAfter
            if after == nil { break }
        }
        return out
    }

    /// The accepted reset's generation for an account, while its continuity
    /// key is still usable; `nil` otherwise.
    static func liveGeneration(_ accountIdB64: String, pins: PeerPinStore = PeerPinStore()) -> UInt64? {
        guard let pin = pins.readPin(accountIdB64: accountIdB64),
              let generation = pin.acceptedKeyGeneration, generation > 0,
              let signedAt = pin.resetSignedAtMs,
              ContinuityTargets.continuityKeyUsable(
                signedAtMs: signedAt, nowMs: Int64(Date().timeIntervalSince1970 * 1000)) else { return nil }
        return generation
    }

    /// For every channel this device publishes or reads, and every reset it
    /// accepted whose window is open, send the returns planned and not yet
    /// sent for that reset. Cards are read only while a live reset exists.
    static func sendReturns(
        channels: ChannelsUiState, client: ChannelsClient, seed: Data, accountIdB64: String
    ) async {
        let pins = PeerPinStore()
        let live = pins.loadSupersededKeys().filter { liveGeneration($0.value, pins: pins) != nil }
        guard !live.isEmpty else { return }
        let ownKeys = StateBundle.ownKeysB64(accountIdB64: accountIdB64)
        let sentStore = SentChannelReturnStore()
        var sent = sentStore.read(accountIdB64: accountIdB64)
        for channel in channels.channels {
            let idHex = channel.id
            let publishing = channels.keysByChannel[idHex] != nil
            let ownRole: ReturnChannel.OwnRole = publishing
                ? (ChannelPublisherLoops.rotates(channelId: idHex, channels: channels) ? .owner : .publisher)
                : .subscriber
            let ownerKeyB64 = ChannelReturnRules.ownerKeyB64(
                extras: channels.extras(idHex), ownRole: ownRole)
            // A subscriber only ever sends an owner return.
            if ownRole == .subscriber, ownerKeyB64.map({ live[$0] == nil }) ?? true { continue }
            guard let cards = try? await openCards(channelId: idHex, channels: channels) else { continue }
            let publishers = CoPublisherKeyBook.publishers(channelIdHex: idHex)
                .compactMap { ChannelsClient.hexToData($0) }.map { base64Encode($0) }
            let planned = ChannelReturnRules.unsent(
                ChannelReturnRules.returnsFor(
                    channels: [ReturnChannel(
                        idHex: idHex, ownRole: ownRole, cards: cards.map(\.card), ownerKeyB64: ownerKeyB64,
                        publisherKeysB64: publishers)],
                    superseded: live, ownKeysB64: ownKeys),
                sent: sent, generationOf: { liveGeneration($0, pins: pins) })
            for ret in planned {
                guard let opened = cards.first(where: { $0.card == ret.card }) else { continue }
                let ok = await sendOne(
                    ret, card: opened, channels: channels, client: client, seed: seed, accountIdB64: accountIdB64)
                if ok, let gen = liveGeneration(ret.accountIdB64, pins: pins) {
                    sent[ChannelReturnRules.sentKey(idHex: ret.idHex, role: ret.role, accountIdB64: ret.accountIdB64)] = gen
                    sentStore.write(sent, accountIdB64: accountIdB64)
                }
            }
        }
    }

    private static func sendOne(
        _ ret: PlannedReturn, card: OpenedCard, channels: ChannelsUiState, client: ChannelsClient,
        seed: Data, accountIdB64: String
    ) async -> Bool {
        guard let pin = PeerPinStore().readPin(accountIdB64: ret.accountIdB64),
              let newKey = base64Decode(pin.pinnedSignaturePubkeyB64),
              await ContinuityTargetRefresh.refresh(
                accountIdB64: ret.accountIdB64, attestedKeyB64: pin.pinnedSignaturePubkeyB64,
                ownAccountIdB64: accountIdB64) != nil,
              let plan = ReachCenter.shared.continuityOnlyPlan(toPersonKey: newKey),
              let channelId = ChannelsClient.hexToData(ret.idHex) else { return false }
        var invitation = Data()
        do {
            switch ret.role {
            case .subscriber:
                // The subscriber card of "invite a contact": one use for a
                // private channel, unlimited for a public one.
                invitation = try await ChannelInvites.invitation(
                    channelIdHex: ret.idHex, state: channels, client: client, currentSeed: seed).card
            case .publisher:
                // A one-use publisher card, recorded so the request it brings
                // is approved without asking (the owner's "Add publisher").
                let identity = try ChannelPublisherIdentity.shared.identity(currentSeed: seed)
                guard let group = try CoPublisher.loadPublisherGroup(identity: identity, channelIdHex: ret.idHex) else {
                    return false
                }
                invitation = try await CoPublisher.mintPublisherLink(
                    identity: identity, publisherGroup: group,
                    channelName: channels.channels.first { $0.id == ret.idHex }?.title,
                    accountIdB64: accountIdB64).card
            case .owner:
                break
            }
        } catch {
            return false
        }
        let seeds = ChannelReturnRules.newestSeeds(
            channels.readSeeds(channelId: ret.idHex).map { (epoch: $0.key, seed: $0.value) })
        do {
            let envelope = try composeChannelReturnEnvelope(ret: FfiChannelReturn(
                channelId: channelId, role: ret.role.ffi, oldBindingSig: card.bindingSig,
                oldSubscriberPk: card.subscriberPk, profileHash: card.profileHash, invitation: invitation,
                epochSeeds: seeds,
                // The person's newest RAW profile held under any of their
                // retired keys, verbatim (their device keeps it only if it
                // verifies under its previous key).
                profile: ChannelReturnRules.returnProfile(
                    accountIdB64: ret.accountIdB64, superseded: PeerPinStore().loadSupersededKeys(),
                    heldEnvelopeOf: { ProfileDirectory.shared.heldEnvelope(signatureKeyB64: $0) })))
            let row = try plan.target.seal(kind: .channelReturn, body: envelope)
            let ds = DsClient()
            _ = try await RefusedMailboxWrite.tryingCandidates(plan.candidates) { auth in
                try await ds.publishMailbox(pseudonym: plan.target.address, payload: row, writeAuth: auth)
            }
        } catch {
            return false
        }
        if ret.role == .publisher, let oldKey = base64Decode(card.card.identityPkB64) {
            // The retired identity's leaves leave the PublisherGroup (a commit
            // and a rotation): its devices are gone, and its key must not
            // publish again. A failure is retried by nobody here.
            do {
                try await CoPublisher.removePublisher(
                    context: ChannelPublisherCommits.Context(
                        channelIdHex: ret.idHex, currentSeed: seed, accountIdB64: accountIdB64,
                        channelsClient: client),
                    state: channels, accountKey: oldKey)
            } catch {
                NSLog("[channels] a reset publisher's old leaves were not removed (%@)",
                      String(describing: type(of: error)))
            }
        }
        return true
    }

    // MARK: - The reset side: returns read from the continuity mailbox

    enum Outcome { case done, retry }

    /// Act on every held return. A return that needs the network and could
    /// not reach it is kept; every other is dropped once handled.
    static func runHeldReturns(
        channels: ChannelsUiState, client: ChannelsClient, seed: Data, accountIdB64: String
    ) async {
        let store = HeldChannelReturnStore()
        let held = store.read(accountIdB64: accountIdB64)
        guard !held.isEmpty else { return }
        let previous = ResetContinuityStore().read(accountIdB64: accountIdB64)?.previousKeyB64
            .flatMap { base64Decode($0) }
        var done: [String] = []
        for envB64 in held {
            guard let env = base64Decode(envB64) else {
                done.append(envB64)
                continue
            }
            var decoded: FfiChannelReturn?
            var verified = false
            if let previous, let ret = try? verifyChannelReturnCardBinding(plaintext: env, previousIdentityPk: previous) {
                decoded = ret
                verified = true
            } else {
                decoded = try? decodeChannelReturnEnvelope(plaintext: env)
            }
            guard let ret = decoded else {
                done.append(envB64)
                continue
            }
            let outcome = await act(
                ret, verified: verified, channels: channels, client: client, seed: seed, accountIdB64: accountIdB64)
            if outcome == .done { done.append(envB64) }
        }
        store.drop(done, accountIdB64: accountIdB64)
    }

    private static func act(
        _ ret: FfiChannelReturn, verified: Bool, channels: ChannelsUiState, client: ChannelsClient,
        seed: Data, accountIdB64: String
    ) async -> Outcome {
        let idHex = ChannelsComposer.bytesToHex(ret.channelId).lowercased()
        let role = ChannelReturnRoleName(ret.role)
        let action = ChannelReturnRules.action(
            verified: verified, role: role, hasInvitation: !ret.invitation.isEmpty,
            alreadyInRole: role == .publisher
                ? channels.keysByChannel[idHex] != nil
                : channels.channels.contains { $0.id == idHex },
            ownsAlready: channels.extras(idHex).accountRole == ChannelAccountRole.owner)
        func addSeeds() {
            let held = channels.subscribedSeeds(channelId: idHex)
            for s in ret.epochSeeds where held[s.epoch] == nil && s.seed.count == 32 {
                channels.setSubscribedSeed(channelId: idHex, epoch: s.epoch, seed: s.seed)
            }
        }
        if verified, let profile = ret.profile {
            ProfileDirectory.shared.offerReturnedOwnProfile(envelope: profile, accountIdB64: accountIdB64)
        }
        switch action {
        case .drop:
            return .done
        case .showInvitation:
            // Shown as an ordinary invitation card; a sealed store keeps it.
            do {
                try ReturnedInvitationStore.shared.keep(
                    channelIdHex: idHex, cardB64: base64Encode(ret.invitation), accountIdB64: accountIdB64)
                return .done
            } catch {
                return .retry
            }
        case .addSeeds:
            addSeeds()
            return .done
        case .acceptPublisher:
            guard let facts = CoPublisherCardFacts.facts(card: ret.invitation), facts.channelIdHex == idHex else {
                return .done
            }
            addSeeds()
            // The ordinary Accept: the request is recorded and knocked, the
            // owner's device approves it (the link was minted for this
            // person), and the Welcome lands through the mailbox. Its own wait
            // is not waited for here; a failure inside the first seconds
            // (offline) keeps the return for the next pass.
            return await acceptWithin(facts: facts, channels: channels, seed: seed)
        case .subscribe:
            guard let facts = ChannelInviteCardFacts.facts(card: ret.invitation), facts.channelIdHex == idHex else {
                return .done
            }
            guard await subscribe(facts: facts, channels: channels, client: client) else { return .retry }
            addSeeds()
            return .done
        case .recoverOwner:
            switch await recoverOwnedChannel(idHex: idHex, channels: channels, client: client, seed: seed,
                                             accountIdB64: accountIdB64) {
            case .owned:
                addSeeds()
                return .done
            case .refused:
                return .done
            case .retry:
                return .retry
            }
        }
    }

    private static func acceptWithin(facts: CoPublisherCardFacts, channels: ChannelsUiState, seed: Data) async -> Outcome {
        let link = facts.link.absoluteString
        let accepting = Task { @MainActor () -> Outcome in
            do {
                _ = try await CoPublisherJoin.run(
                    link: link, localName: facts.channelName, state: channels, accountIdentitySeed: seed,
                    onStage: { _ in })
                return .done
            } catch {
                return .retry
            }
        }
        let timer = Task { @MainActor () -> Outcome in
            try? await Task.sleep(nanoseconds: acceptSettleSeconds * 1_000_000_000)
            return .done
        }
        return await withTaskGroup(of: Outcome.self) { group in
            group.addTask { await accepting.value }
            group.addTask { await timer.value }
            let first = await group.next() ?? .done
            timer.cancel()
            group.cancelAll()
            return first
        }
    }

    /// Subscribe with an invitation card, the way `Subscribe` on the card
    /// does (``ChannelConversationModel/subscribe()``), without a screen.
    static func subscribe(facts: ChannelLinkFacts, channels: ChannelsUiState, client: ChannelsClient) async -> Bool {
        let idHex = facts.channelIdHex
        channels.noteInviter(facts)
        if !channels.channels.contains(where: { $0.id == idHex }) {
            channels.addChannel(ChannelHandle(
                id: idHex, localName: facts.name, localDescription: facts.description,
                addedAtMs: UInt64(Date().timeIntervalSince1970 * 1000), handle: ""))
        }
        channels.onSubscribedHere?(idHex)
        let keys = channels.subscriberKeysForSubscribe(idHex)
        let pubHex = ChannelsComposer.bytesToHex(keys.publicKey)
        do {
            switch facts.access {
            case .public(let readKey):
                channels.updateExtras(idHex) { $0.readKey = readKey }
                _ = try await client.subscribe(channelIdHex: idHex, subscriberPubkeyHex: pubHex)
            case .private(let inviteSeed, let admissionPublicKey, let admissionVersion):
                guard let id = ChannelsClient.hexToData(idHex) else { return false }
                let signer = try ChannelInviteSigner.fromInviteSeed(inviteSeed: inviteSeed)
                let ticket = try signer.sealTicket(
                    admissionPublicKey: admissionPublicKey, admissionVersion: admissionVersion,
                    channelId: id, subscriberPk: keys.publicKey)
                let proof = try signer.signProof(channelId: id, subscriberPk: keys.publicKey)
                _ = try await client.subscribePrivate(
                    channelIdHex: idHex, subscriberPubkeyHex: pubHex, inviteProof: proof, sealedTicket: ticket)
                channels.updateExtras(idHex) { $0.waitingForAdmission = true }
            }
            return true
        } catch {
            return false
        }
    }

    enum RecoveryOutcome { case owned, refused, retry }

    /// Take a channel back with its recovery key: the key evaluated again
    /// for this account, the owner key moved to the current identity's with
    /// the recovery proof, and on `204` the channel goes into the list and
    /// the register as owned, every reader the service lists stays let in,
    /// and a fresh admission seed is minted; the publisher pass then founds
    /// its PublisherGroup again (ADR-0090 decision A) and reseals. `401`:
    /// not this account's, dropped silently.
    static func recoverOwnedChannel(
        idHex: String, channels: ChannelsUiState, client: ChannelsClient, seed: Data, accountIdB64: String
    ) async -> RecoveryOutcome {
        guard let identity = try? Identity.fromSeed(seed: seed), let channelId = ChannelsClient.hexToData(idHex) else {
            return .retry
        }
        let key: ChannelRecoveryKey
        do {
            key = try await deriveRecoveryKey(idHex: idHex)
        } catch is RecoveryDisabled {
            return .refused
        } catch {
            return .retry
        }
        guard let newOwnerPk = try? ChannelsClient.ownerSigner(accountIdentity: identity, channelIdHex: idHex)
                .verifyingKey(),
              let header = try? key.signOwnerChange(
                channelId: channelId, newOwnerPk: newOwnerPk, nowMs: UInt64(Date().timeIntervalSince1970 * 1000))
        else { return .retry }
        let status = await client.putOwnerByRecovery(
            channelIdHex: idHex, recoveryProofHeader: header, newOwnerPk: newOwnerPk)
        guard let status, status < 500 else { return .retry }
        guard status == 204 else { return .refused }
        NSLog("ADR-0101: a channel this account owned was taken back with its recovery key")
        if !channels.channels.contains(where: { $0.id == idHex }) {
            channels.addChannel(ChannelHandle(
                id: idHex, localName: "", localDescription: "",
                addedAtMs: UInt64(Date().timeIntervalSince1970 * 1000), handle: ""))
        }
        // Every reader the service lists now stays let in: the re-founding's
        // reseal reaches each of them.
        var through: UInt64 = 0
        var after: UInt64 = 0
        for _ in 0..<10_000 {
            guard let page = try? await client.subscriberPage(
                channelIdHex: idHex, after: after, accountIdentity: identity) else { break }
            through = RecoveredChannelRules.highestSeq(page.rows.map(\.seq), held: through)
            guard !page.rows.isEmpty, page.nextAfter > after else { break }
            after = page.nextAfter
        }
        RecoveredChannelStore().mark(idHex, throughSeq: through, accountIdB64: accountIdB64)
        // Registered again under the new owner key at the next pass.
        ChannelRecoveryRegisteredStore().unmark(idHex, accountIdB64: accountIdB64)
        let mark = through
        channels.updateExtras(idHex) {
            $0.admittedThroughSeq = max($0.admittedThroughSeq ?? 0, mark)
            $0.admissionMarkSet = true
            $0.subscriberRowsReadSeq = max($0.subscriberRowsReadSeq ?? 0, mark)
            $0.accountRole = ChannelAccountRole.owner
        }
        // A fresh admission seed verifies the tickets of invitations made
        // from now on (version 1: the lost version 0 is never reused).
        if ChannelInvites.admissionSeeds(channelIdHex: idHex).isEmpty {
            ChannelInvites.putAdmissionSeed(channelIdHex: idHex, version: 1, seed: generateChannelAdmissionSeed())
            ChannelDeviceAdmission.syncAdmissionSeeds(channelId: idHex)
        }
        // The account's one subscriber key, and the subscription register
        // as owner.
        _ = channels.subscriberKeysForSubscribe(idHex)
        ChannelSubscriptionSync.shared.reconcileNow()
        return .owned
    }

    /// A recovered channel takes its type from its newest metadata record
    /// once one is held (a public channel's read key is the one the current
    /// account seed derives), and the recovery mark goes.
    static func settleRecovered(channels: ChannelsUiState, seed: Data, accountIdB64: String) {
        let store = RecoveredChannelStore()
        let recovered = store.read(accountIdB64: accountIdB64)
        guard !recovered.isEmpty, let lineage = try? Identity.fromSeed(seed: seed) else { return }
        for idHex in recovered.keys {
            let extras = channels.extras(idHex)
            guard let visibility = RecoveredChannelRules.visibility(metaListed: extras.recordPublic ?? extras.isPublic)
            else { continue }
            if visibility == .publicChannel, extras.readKey == nil,
               let channelId = ChannelsClient.hexToData(idHex),
               let readKey = try? channelReadKey(ownerAccountIdentity: lineage, channelId: channelId) {
                channels.updateExtras(idHex) { $0.readKey = readKey }
            }
            store.clear(idHex, accountIdB64: accountIdB64)
        }
        // The returned invitations of channels this account now reads leave.
        let reading = Set(channels.channels.map(\.id))
        let held = ReturnedInvitationStore.shared.read(accountIdB64: accountIdB64)
        let read = held.keys.filter { reading.contains($0) }
        if !read.isEmpty { ReturnedInvitationStore.shared.drop(channelIdsHex: Array(read), accountIdB64: accountIdB64) }
    }
}
