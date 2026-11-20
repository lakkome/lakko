// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The account's one subscriber key on the publishing device (2026-11-13,
/// follow-up decision 1). Mirror of web `channels/account-reader-key.ts`.
///
/// The channel subscription register carries one subscriber key per account
/// and channel, also for an owner or a co-publisher. Its other devices read
/// the channel with it, so it must be a real reader key at the channels
/// service: the device that holds the PublisherGroup registers it there as a
/// subscriber (once; also for a channel whose register carried a key the
/// service did not know yet), hands it the current seed, and every rotation
/// it makes reseals the new seed to it (``ChannelReseal``, on a private
/// channel too, whatever the admission mark says).
///
/// On a private channel the service wants an invite proof and a sealed
/// admission ticket for a new row. This device mints a one-use invite key of
/// its own for it and keeps it registered (it never expires), so the ticket
/// stays valid and the let-in pass of the device holding the mark admits the
/// row like any invited reader's, rather than removing it.
@MainActor
enum ChannelAccountReaderKey {

    /// The longest wait after a transient failure (2026-11-20: the
    /// ``PublisherCommitHold`` schedule, 30 s doubling to this; a refusal
    /// waits for the next start).
    static let retryAfterMs: UInt64 = UInt64(SweepRegistrationBackoff.maxDelayMs)

    private static var holds: [String: PublisherCommitHold] = [:]

    enum Step: Equatable { case register, nothing }

    /// What has to happen for the account's reader key on this device. Pure.
    nonisolated static func step(
        publishing: Bool, keyHex: String?, registeredHex: String?, hold: PublisherCommitHold?, nowMs: UInt64
    ) -> Step {
        guard publishing, let keyHex else { return .nothing }
        if registeredHex?.lowercased() == keyHex.lowercased() { return .nothing }
        if let hold, !hold.allows(nowMs: Int64(clamping: nowMs)) { return .nothing }
        return .register
    }

    /// The registration as this device makes it, a seam for tests.
    struct Ports {
        var subscribePublic: (_ channelId: String, _ keyHex: String) async throws -> Void
        var subscribePrivate: (_ channelId: String, _ keyHex: String) async throws -> Void
        var handCurrentSeed: (_ channelId: String, _ keyHex: String) async throws -> Void
    }

    /// Forget the failure times; on sign-out and at a new session.
    static func reset() { holds = [:] }

    /// Whether the registration of `channelId` was refused (2026-11-20).
    static func isRefused(channelId: String) -> Bool { holds[channelId]?.refused == true }

    /// Drop the hold of `channelId` (the owner key was repaired).
    static func clearHold(channelId: String) { holds[channelId] = nil }

    /// Register the account's reader key for a channel this device publishes
    /// in, once, and hand it the current seed. Best effort: a failure is
    /// retried by a later publisher pass (after the hold). Never
    /// throws.
    static func ensure(
        channelId: String, state: ChannelsUiState, keyHex: String?, ports: Ports,
        nowMs: UInt64 = UInt64(Date().timeIntervalSince1970 * 1000)
    ) async {
        let decision = step(
            publishing: state.keysByChannel[channelId] != nil, keyHex: keyHex,
            registeredHex: state.extras(channelId).readerKeyRegisteredHex,
            hold: holds[channelId], nowMs: nowMs)
        guard decision == .register, let keyHex = keyHex?.lowercased() else { return }
        // The flag is not known yet (the creating screen writes the first
        // record just after the keys): a public subscribe of a private
        // channel would only fail. The next pass decides.
        guard let isPublic = state.extras(channelId).isPublic else { return }
        do {
            if !isPublic {
                try await ports.subscribePrivate(channelId, keyHex)
            } else {
                try await ports.subscribePublic(channelId, keyHex)
            }
            holds[channelId] = nil
        } catch {
            // 2026-11-20 — a refusal (`401 owner-proof-required`) waits for
            // the next start; anything else 30 s, doubling to 10 minutes.
            let hold = PublisherCommitHold.after(
                holds[channelId], failure: .of(error), nowMs: Int64(clamping: nowMs))
            holds[channelId] = hold
            NSLog("[channels] the account's reader key was not registered (%@); %@",
                  PublisherCommitFailure.logLabel(error),
                  hold.refused ? "not tried again until the next start" : "tried again after a wait")
            return
        }
        // Lost before it is written (a crash), the next pass registers again:
        // a row already there takes no invite use.
        state.updateExtras(channelId) { $0.readerKeyRegisteredHex = keyHex }
        // The current seed now, not at the next rotation. A failure is the
        // admission pass's and the next rotation's to cover.
        try? await ports.handCurrentSeed(channelId, keyHex)
    }

    /// The account's register key of `channelId` as lower-case hex, or `nil`.
    static func keyHex(channelId: String) -> String? {
        ChannelSubscriptionSync.shared.registeredKeys(channelId: channelId)
            .map { ChannelsComposer.bytesToHex($0.publicKey).lowercased() }
    }

    /// The ports the app uses: the channels service and the admission pass.
    static func livePorts(state: ChannelsUiState, client: ChannelsClient, currentSeed: Data) -> Ports {
        Ports(
            subscribePublic: { channelId, keyHex in
                _ = try await client.subscribe(channelIdHex: channelId, subscriberPubkeyHex: keyHex)
            },
            subscribePrivate: { channelId, keyHex in
                // Security pass 2026-11-15 (F1) — never a mark here: with no
                // seed held, the registration waits (``admissionForReaderKey``).
                try await subscribePrivate(
                    channelId: channelId, keyHex: keyHex, client: client, currentSeed: currentSeed,
                    admission: admissionForReaderKey(channelId: channelId, state: state))
            },
            handCurrentSeed: { channelId, keyHex in
                try await SubscriberAdmission.shared.admit(
                    channelId: channelId, pubkeyHex: keyHex, state: state,
                    accountIdentity: try Identity.fromSeed(seed: currentSeed))
            })
    }

    struct NoAdmissionSeed: Error {}

    /// The admission seed the reader key's ticket is sealed to, or `nil`.
    /// Never minted here (security pass 2026-11-15, F1): a new private
    /// channel gets version 0 at creation (``ChannelInvites/mintAtCreation(channelIdHex:)``)
    /// and a public-to-private switch mints it on the switching device; a
    /// device that holds none for an existing private channel (an owner
    /// device the account's admission-seed register has not reached yet, or
    /// a co-publisher) waits for the register, and this registration is
    /// tried again after ``retryAfterMs``. Minting here used to mark every
    /// row the service listed as a reader, so planted rows got the seed.
    static func admissionForReaderKey(channelId: String, state: ChannelsUiState) -> (seed: Data, version: UInt16)? {
        let holds = !ChannelInvites.admissionSeeds(channelIdHex: channelId).isEmpty
        return ChannelInvites.admissionSeed(
            channelIdHex: channelId,
            mintIfMissing: ChannelInvites.admissionSeedAction(.later, holdsSeed: holds).mint)
    }

    /// A private channel: a one-use invite key that never expires (kept
    /// registered, never deleted), an admission ticket sealed to the newest
    /// admission seed, and the invite proof.
    private static func subscribePrivate(
        channelId: String, keyHex: String, client: ChannelsClient, currentSeed: Data,
        admission: (seed: Data, version: UInt16)?
    ) async throws {
        guard let admission else { throw NoAdmissionSeed() }
        guard let channelBytes = ChannelsClient.hexToData(channelId),
              let subscriber = ChannelsClient.hexToData(keyHex) else { throw NoAdmissionSeed() }
        let identity = try Identity.fromSeed(seed: currentSeed)
        var inviteSeed = generateChannelInviteSeed()
        defer { inviteSeed.resetBytes(in: 0..<inviteSeed.count) }
        let signer = try ChannelInviteSigner.fromInviteSeed(inviteSeed: inviteSeed)
        // One use, never expiring: the row's ticket names it for good.
        try await client.putInviteKey(
            channelIdHex: channelId, invitePublicKey: signer.verifyingKey(), expiryMs: 0, maxUses: 1,
            accountIdentity: identity)
        let ticket = try signer.sealTicket(
            admissionPublicKey: try channelAdmissionPublicKey(admissionSeed: admission.seed),
            admissionVersion: admission.version, channelId: channelBytes, subscriberPk: subscriber)
        let proof = try signer.signProof(channelId: channelBytes, subscriberPk: subscriber)
        _ = try await client.subscribePrivate(
            channelIdHex: channelId, subscriberPubkeyHex: keyHex, inviteProof: proof, sealedTicket: ticket)
    }
}
