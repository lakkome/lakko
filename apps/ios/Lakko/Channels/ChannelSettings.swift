// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0087 sections 3, 8, 11 and 16 — what the owner of a channel changes
/// about it: its `@name` and whether it is public, its topic and language.
/// One place for the creation path and the Channel type screen.
///
/// Order, for a change: discovery first (a held name answers there and
/// nothing else moves), then the channels service's private flag, then the
/// metadata record with the signed handle block (every subscriber learns the
/// flag and the name from it), then the Discover listing (public means
/// listed, section 16). Going private starts a new epoch at once, so the
/// public seed of the old one opens nothing current.
@MainActor
enum ChannelSettings {

    struct TypeChange: Equatable {
        var isPublic: Bool
        var handle: String
        var topic: UInt8
        var language: String
    }

    /// The `@name` this account holds for `channelId`, from the held-names
    /// store (the owner's own record of what discovery accepted).
    static func heldHandle(channelId: String) -> HeldHandleStore.Entry? {
        guard let account = AcceptedContactsStore.boundAccountIdB64 else { return nil }
        return HeldHandleStore.load(account).first {
            $0.kind == "channel" && $0.channelIdHex?.lowercased() == channelId.lowercased()
        }
    }

    /// Claim, replace or update the channel's name at discovery and sign the
    /// handle block the metadata record carries.
    static func writeHandle(
        channelId: String, state: ChannelsUiState, currentSeed: Data, change: TypeChange
    ) async throws -> FfiConversationHandle {
        guard let account = AcceptedContactsStore.boundAccountIdB64,
              let id = ChannelsClient.hexToData(channelId) else { throw ChannelMetaError.notAPublisher }
        let current = try Identity.fromSeed(seed: currentSeed)
        let lineage = ChannelLineage.identity(accountIdB64: account, currentSeed: currentSeed) ?? current
        let channel = state.channels.first { $0.id == channelId }
        let name = channel?.title ?? ""
        let description = channel?.displayDescription ?? ""
        let (journal, facts) = try ConversationHandles.channelFacts(
            lineageIdentity: lineage, channelId: id, name: name, description: description,
            avatar: state.extras(channelId).avatar?.ffi,
            size: state.subscriberCountByChannel[channelId] ?? 0)
        let accountSeed = current.ed25519Seed()
        let wanted = ConversationHandles.canonical(change.handle)
        let held = heldHandle(channelId: channelId)
        let written: ConversationHandles.Written
        let discovery = DiscoveryClient()
        if let held, held.handle != wanted {
            written = try await ConversationHandles.replace(
                discovery: discovery, accountSeed: accountSeed, journalId: journal,
                oldHandle: held.handle, newHandle: wanted, isPublic: change.isPublic,
                facts: change.isPublic ? facts : nil)
        } else if held != nil {
            written = try await ConversationHandles.update(
                discovery: discovery, accountSeed: accountSeed, journalId: journal,
                handle: wanted, isPublic: change.isPublic, facts: change.isPublic ? facts : nil)
        } else {
            written = try await ConversationHandles.claim(
                discovery: discovery, accountSeed: accountSeed, journalId: journal,
                handle: wanted, isPublic: change.isPublic, facts: change.isPublic ? facts : nil)
        }
        HeldHandleStore.put(HeldHandleStore.Entry(
            kind: "channel", handle: written.handle, isPublic: change.isPublic,
            journalIdHex: AttachmentBridge.hex(journal), channelIdHex: channelId.lowercased(),
            name: name, description: description, leaseUntilMs: written.leaseUntilMs), account)
        HandleTakenCheck.note(written.handle, .taken)
        var seed = try ConversationHandles.seed(accountSeed: accountSeed, journalId: journal)
        defer { RootHandover.wipe(&seed) }
        let value = try signConversationHandle(
            handleSeed: seed, groupId: journal, updatedAtMs: UInt64(Date().timeIntervalSince1970 * 1000),
            public: change.isPublic, handle: written.handle, certificate: written.certificate)
        state.updateLocalMeta(channelId: channelId, handle: written.handle)
        if change.isPublic {
            state.updateExtras(channelId) { $0.readKey = facts.key }
        }
        return value
    }

    /// Section 8 — apply a Channel type change, or the first `@name` of a new
    /// channel (`creating`).
    static func saveType(
        channelId: String, state: ChannelsUiState, client: ChannelsClient, currentSeed: Data,
        change: TypeChange, creating: Bool = false
    ) async throws {
        let identity = try Identity.fromSeed(seed: currentSeed)
        let wasPublic = state.extras(channelId).isPublic ?? false
        let handle = try await writeHandle(
            channelId: channelId, state: state, currentSeed: currentSeed, change: change)
        if creating || wasPublic != change.isPublic {
            try await client.setPrivate(channelIdHex: channelId, isPrivate: !change.isPublic, accountIdentity: identity)
        }
        // Section 4.1 — going private: "Everyone subscribed stays". The rows
        // of today are readers on this device's mark (the web type change),
        // so the let-in pass never removes them for having no ticket.
        if wasPublic, !change.isPublic {
            try await SubscriberAdmission.markCurrentRowsAsReaders(
                channelId: channelId, state: state, client: client, accountIdentity: identity)
        }
        let channel = state.channels.first { $0.id == channelId }
        let held = state.sharedMeta(channelId: channelId)
        let now = max(UInt64(Date().timeIntervalSince1970 * 1000), (held?.updatedAtMs ?? 0) &+ 1)
        let meta = SharedChannelMeta(
            name: held?.name ?? channel?.title ?? "",
            description: held?.description ?? channel?.displayDescription ?? "",
            updatedAtMs: now, topic: change.topic, language: change.language, listed: change.isPublic,
            handle: handle, avatar: held?.avatar ?? state.extras(channelId).avatar?.ffi,
            reactions: held?.reactions)
        try await ChannelMetaRecord.post(
            state: state, client: client, channelId: channelId, meta: meta, accountIdentity: identity)
        // Section 16 — public means listed; a private channel is never listed.
        if change.isPublic {
            try? await ChannelListing.list(
                state: state, client: client, channelIdHex: channelId, identity: identity,
                topic: change.topic,
                language: change.language.isEmpty ? DiscoverLanguages.appLanguage() : change.language)
        } else if wasPublic {
            try? await client.deleteListing(channelIdHex: channelId, accountIdentity: identity)
            // Section 8 — a new epoch, the seed sealed to each subscriber.
            try? await ChannelPublisherLoops.shared.rotateNow(
                channelId: channelId, channels: state, client: client, seed: currentSeed)
        }
    }

    /// The line a refused save shows (UX section 3.3 / 4.1).
    static func errorKey(_ error: Error) -> String {
        if let refusal = error as? DiscoveryClient.HandleWriteError {
            if refusal.code == "name-taken" { return "group_handle.error.held" }
            if refusal.status == 429 { return "common.error.too_many_attempts" }
            return "group_handle.error.failed"
        }
        if case DiscoveryClientError.network = error { return "group_handle.error.offline" }
        if error is AnonTokenUnavailable { return "group_handle.error.offline" }
        return "group_handle.error.failed"
    }
}
