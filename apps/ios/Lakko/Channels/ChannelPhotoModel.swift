// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0087 section 18.2 / UX 4.5.2 — a channel's photo on its info screen:
/// only the owner sets it, `Use` uploads and sends it in a new metadata
/// record (every subscriber reads it with the name), `Remove` sends a record
/// without one. A replaced photo this device uploaded is deleted.
@MainActor
final class ChannelPhotoModel: ObservableObject, ConversationPhotoModel {
    let channelId: String
    let state: ChannelsUiState
    let client: ChannelsClient
    let accountIdentitySeed: Data

    @Published var photoStatus: ChatViewModel.GroupPhotoStatus = .idle
    private var retry: PendingAvatarMedia?

    init(channelId: String, state: ChannelsUiState, client: ChannelsClient, accountIdentitySeed: Data) {
        self.channelId = channelId
        self.state = state
        self.client = client
        self.accountIdentitySeed = accountIdentitySeed
    }

    var photoTitle: String { state.channels.first { $0.id == channelId }?.title ?? "" }
    var photoIdHex: String { channelId }
    var photoIsGroup: Bool { false }
    /// Section 4.5.1 — the owner (who renames it).
    var canSetPhoto: Bool { ChannelOwnership.isOwner(channelId: channelId, state: state) }
    var currentPhoto: ProfileAvatar? { state.extras(channelId).avatar }
    var photoRemoveTitleKey: String { "channels.photo.remove_title" }
    var photoRemoveBodyKey: String { "channels.photo.remove_body" }
    var photoNotAllowedKey: String { "channels.photo.only_owner" }

    func usePhoto(_ media: PendingAvatarMedia) {
        guard canSetPhoto else {
            photoStatus = .notAllowed
            return
        }
        retry = media
        photoStatus = .uploading
        Task { @MainActor in
            let avatar: ProfileAvatar
            do {
                avatar = try await ProfileAvatarUpload.upload(media)
            } catch AttachmentBridgeError.uploadLimited {
                photoStatus = .limited
                return
            } catch {
                photoStatus = .failed
                return
            }
            if await send(avatar) {
                retry = nil
            } else {
                BlobLifecycle.shared.delete(Self.blobs(avatar))
            }
        }
    }

    func retryPhoto() {
        if let retry { usePhoto(retry) }
    }

    func removePhoto() {
        guard canSetPhoto else {
            photoStatus = .notAllowed
            return
        }
        Task { @MainActor in _ = await send(nil) }
    }

    /// A new record with the photo (or none), its stamp strictly newer.
    private func send(_ avatar: ProfileAvatar?) async -> Bool {
        guard let identity = try? Identity.fromSeed(seed: accountIdentitySeed),
              let held = state.sharedMeta(channelId: channelId) else {
            photoStatus = .failed
            return false
        }
        let previous = held.avatar.flatMap(ProfileAvatar.init)
        let now = max(UInt64(Date().timeIntervalSince1970 * 1000), held.updatedAtMs &+ 1)
        do {
            try await ChannelMetaRecord.post(
                state: state, client: client, channelId: channelId,
                meta: held.with(updatedAtMs: now, avatar: .some(avatar?.ffi)), accountIdentity: identity)
            photoStatus = .idle
            if let previous, previous != avatar { BlobLifecycle.shared.delete(Self.blobs(previous)) }
            return true
        } catch {
            photoStatus = .failed
            return false
        }
    }

    private static func blobs(_ avatar: ProfileAvatar) -> [Data] {
        [avatar.still.blobId] + (avatar.video.map { [$0.blobId] } ?? [])
    }
}

/// Whether this device owns a channel: it registered it here, or holds its
/// `@name` (only the owner ever does).
enum ChannelOwnership {
    @MainActor
    /// This device publishes the channel and is not a co-publisher of it (a
    /// co-published channel is recorded with its PublisherGroup id when it is
    /// joined). Holds for a channel made before ADR-0087 phase B too, which
    /// recorded no owner generation and no @name.
    ///
    /// A co-publisher's register role (ADR-0050 section 2.9 as amended
    /// 2026-11-13) never reads as the owner either, whatever keys are held.
    static func isOwner(channelId: String, state: ChannelsUiState) -> Bool {
        state.keysByChannel[channelId] != nil
            && CoPublishedJournals.shared.journalIdHex(channelIdHex: channelId) == nil
            && state.extras(channelId).accountRole != ChannelAccountRole.coPublisher
    }
}

/// Reactions-and-emoji spec section 9 / ADR-0087 section 13a — the reactions
/// a channel's owner allows (all, none, or a list), carried in its metadata
/// record so every reader and the composer hide what is not allowed.
@MainActor
final class ChannelReactionsSetting: ObservableObject, AllowedReactionsModel {
    let channelId: String
    let state: ChannelsUiState
    let client: ChannelsClient
    let accountIdentitySeed: Data

    init(channelId: String, state: ChannelsUiState, client: ChannelsClient, accountIdentitySeed: Data) {
        self.channelId = channelId
        self.state = state
        self.client = client
        self.accountIdentitySeed = accountIdentitySeed
    }

    var allowedReactions: FfiAllowedReactions { state.extras(channelId).allowed }

    func saveAllowedReactions(_ value: FfiAllowedReactions) async -> Bool {
        guard (try? validateAllowedReactions(value: value)) != nil,
              let identity = try? Identity.fromSeed(seed: accountIdentitySeed),
              let held = state.sharedMeta(channelId: channelId) else { return false }
        let now = max(UInt64(Date().timeIntervalSince1970 * 1000), held.updatedAtMs &+ 1)
        let reactions: FfiAllowedReactions? = value == .all ? nil : value
        do {
            try await ChannelMetaRecord.post(
                state: state, client: client, channelId: channelId,
                meta: held.with(updatedAtMs: now, reactions: .some(reactions)), accountIdentity: identity)
            objectWillChange.send()
            return true
        } catch {
            return false
        }
    }

    /// The Info row's value.
    var summary: String {
        switch allowedReactions {
        case .all: return Localization.text("group_reactions.value.all")
        case .off: return Localization.text("group_reactions.value.off")
        case .only(let list):
            return Localization.text("group_reactions.value.count", counts: ["count": UInt64(list.count)])
        }
    }
}
