// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

/// The production ports of Settings > Profile.
extension ProfileEditModel {
    @MainActor
    static func live(
        authRepository: AuthRepository,
        account: SessionAccount,
        protection: AccountProtectionCenter? = nil,
        directory: ProfileDirectory? = nil,
        republish: @escaping @MainActor () async -> Void
    ) -> ProfileEditModel {
        let directory = directory ?? .shared
        let username: (String?) -> String? = { name in
            guard let name, !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return nil }
            return name
        }
        let saved = ProfileSaved(
            avatar: directory.ownProfile?.avatar,
            name: directory.ownProfile?.displayName,
            username: username(directory.ownUsername ?? account.username))
        let changes = directory.$ownProfile
            .combineLatest(directory.$ownUsername)
            .map { profile, name in
                ProfileSaved(avatar: profile?.avatar, name: profile?.displayName, username: username(name))
            }
            .eraseToAnyPublisher()
        let ports = ProfileSavePorts(
            upload: { media in try await ProfileAvatarUpload.upload(media) },
            rename: { typed in try await authRepository.renameUsername(to: typed) },
            publish: { name, avatar in
                try ProfileAvatarUpload.checkComposes(name: name, avatar: avatar)
                let previous = directory.ownProfile?.avatar
                try directory.setOwnProfile(displayName: name ?? "", avatar: avatar)
                await republish()
                // ADR-0065 section 6 — a new or removed photo: the previous
                // still and video are deleted (owed and retried on failure).
                BlobLifecycle.shared.delete(ProfileAvatarUpload.replacedBlobs(previous: previous, current: avatar))
            },
            renameOwed: { authRepository.hasOwedRename },
            finishOwedRename: { await authRepository.finishOwedRename() },
            renameTurnsOffRecoveryEmail: {
                guard let protection else { return false }
                return UsernameRename.warnings(
                    recoveryEmailEnabled: protection.status?.enabled == true,
                    addressHeld: protection.addressHeld != nil
                ).recoveryEmailTurnedOff
            })
        return ProfileEditModel(saved: saved, ports: ports, savedChanges: changes)
    }
}

/// ADR-0084 section 1 — the still and the video, each encrypted under its own
/// fresh key and uploaded as an attachment blob.
enum ProfileAvatarUpload {
    @MainActor
    static func upload(_ media: PendingAvatarMedia, storage: StorageClient = StorageClient()) async throws -> ProfileAvatar {
        let still = try await AttachmentBridge.encryptAndUpload(
            bytes: media.stillJPEG, mime: nil, name: nil, storage: storage)
        let stillRef = AvatarBlobRef(
            blobId: still.blobId, key: still.key, nonce: still.nonce,
            size: UInt32(media.stillJPEG.count), side: UInt16(clamping: media.stillSide))
        var videoRef: AvatarBlobRef?
        if let file = media.videoFile, let side = media.videoSide {
            let bytes = try Data(contentsOf: file)
            let video: AttachmentRef
            do {
                video = try await AttachmentBridge.encryptAndUpload(bytes: bytes, mime: nil, name: nil, storage: storage)
            } catch {
                // The still went up and will never be referenced.
                BlobLifecycle.shared.delete([still.blobId])
                throw error
            }
            videoRef = AvatarBlobRef(
                blobId: video.blobId, key: video.key, nonce: video.nonce,
                size: UInt32(bytes.count), side: UInt16(clamping: side))
        }
        let avatar = ProfileAvatar(
            still: stillRef, video: videoRef,
            videoDurationMs: media.videoDurationMs.map { UInt16(clamping: $0) })
        // The own avatar shows at once on every surface, without a download.
        AvatarImages.shared.adopt(stillJPEG: media.stillJPEG, videoFile: media.videoFile, avatar: avatar)
        return avatar
    }

    /// The blobs of `previous` that `current` no longer references.
    static func replacedBlobs(previous: ProfileAvatar?, current: ProfileAvatar?) -> [Data] {
        let old = [previous?.still.blobId, previous?.video?.blobId].compactMap { $0 }
        let kept = Set([current?.still.blobId, current?.video?.blobId].compactMap { $0 })
        return old.filter { !kept.contains($0) }
    }

    /// Refuse a profile the core would refuse, before anything is stored:
    /// a published profile that no envelope can carry would reach nobody.
    ///
    /// The envelope is thrown away, so it is signed (profile v6, ADR-0101
    /// section 8) by a fixed throwaway identity: the signature never decides
    /// whether the core accepts a profile.
    static func checkComposes(name: String?, avatar: ProfileAvatar?) throws {
        let normalized = try checkDisplayName(name ?? "")
        _ = try composeProfileEnvelope(profile: FfiProfile(
            displayName: normalized, username: nil, updatedAtMs: 1, accountId: nil,
            usernameCertificate: nil, grants: [], avatar: avatar?.ffi),
            accountIdentity: try Identity.fromSeed(seed: Data(repeating: 0x5A, count: 32)))
    }
}
