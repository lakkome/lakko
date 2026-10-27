// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0084 — one encrypted avatar blob: what `encryptAttachment` returned for
/// it, its plaintext size and its side in pixels (the picture is square).
///
/// `key` is SECRET: it opens the blob. It is kept only beside the profile it
/// arrived in (app-private storage, like the profile itself) and is never
/// logged (`description` leaves it out).
public struct AvatarBlobRef: Codable, Hashable, Sendable, CustomStringConvertible {
    public let blobId: Data
    public let key: Data
    public let nonce: Data
    public let size: UInt32
    public let side: UInt16

    public init(blobId: Data, key: Data, nonce: Data, size: UInt32, side: UInt16) {
        self.blobId = blobId
        self.key = key
        self.nonce = nonce
        self.size = size
        self.side = side
    }

    /// Lower-case hex of the blob id: the cache file name and the storage path.
    public var blobIdHex: String { AttachmentBridge.hex(blobId) }

    public var description: String { "AvatarBlobRef(size: \(size), side: \(side))" }

    var ffi: FfiAvatarRef {
        FfiAvatarRef(blobId: blobId, key: key, nonce: nonce, size: size, side: side)
    }

    init(_ ffi: FfiAvatarRef) {
        self.init(blobId: ffi.blobId, key: ffi.key, nonce: ffi.nonce, size: ffi.size, side: ffi.side)
    }

    /// The attachment ref the download path opens it with.
    var attachmentRef: AttachmentRef {
        AttachmentRef(
            blobId: blobId, key: key, nonce: nonce, mime: nil, size: UInt64(size), name: nil)
    }
}

/// ADR-0084 — a person's profile photo: a square JPEG still and, for a
/// profile video, a square MP4 (no sound) whose first frame the still is.
public struct ProfileAvatar: Codable, Hashable, Sendable {
    public let still: AvatarBlobRef
    public let video: AvatarBlobRef?
    /// Set exactly when `video` is.
    public let videoDurationMs: UInt16?

    public init(still: AvatarBlobRef, video: AvatarBlobRef? = nil, videoDurationMs: UInt16? = nil) {
        self.still = still
        self.video = video
        self.videoDurationMs = video == nil ? nil : videoDurationMs
    }

    var ffi: FfiAvatar {
        FfiAvatar(still: still.ffi, video: video?.ffi, videoDurationMs: video == nil ? nil : videoDurationMs)
    }

    init?(_ ffi: FfiAvatar?) {
        guard let ffi else { return nil }
        self.init(
            still: AvatarBlobRef(ffi.still),
            video: ffi.video.map(AvatarBlobRef.init),
            videoDurationMs: ffi.videoDurationMs)
    }
}

/// ADR-0084 section 2 — the bounds the core refuses a whole envelope outside
/// of. The client produces media inside them; the core is the authority.
public enum AvatarLimits {
    /// The still: a square JPEG, 1...640 px a side.
    public static let stillMaxSide = 640
    /// The still: 1...262144 bytes.
    public static let stillMaxBytes = 262_144
    /// The video: a square H.264 MP4, 1...480 px a side.
    public static let videoMaxSide = 480
    /// The video: 1...4194304 bytes.
    public static let videoMaxBytes = 4_194_304
    /// The video: 1...10000 ms.
    public static let videoMaxDurationMs = 10_000
}
