// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Cross-device sync state types (Phase 2a3e2b-iOS).
///
/// Mirror of `apps/web/src/devices/sync/state.ts`. Four record
/// shapes — one per typed body variant in
/// `schemas/mls-app/devicegroup_v1.proto` — plus a per-record
/// `updatedAtHlc` for last-write-wins resolution.

public struct DraftState: Equatable {
    public let text: String
    public let replyToId: Data
    public let updatedAtHlc: UInt64

    public init(text: String, replyToId: Data, updatedAtHlc: UInt64) {
        self.text = text
        self.replyToId = replyToId
        self.updatedAtHlc = updatedAtHlc
    }
}

public struct FlagsState: Equatable {
    public let pinned: Bool
    public let muted: Bool
    public let archived: Bool
    public let unreadOverride: Bool
    public let updatedAtHlc: UInt64

    public init(
        pinned: Bool,
        muted: Bool,
        archived: Bool,
        unreadOverride: Bool,
        updatedAtHlc: UInt64
    ) {
        self.pinned = pinned
        self.muted = muted
        self.archived = archived
        self.unreadOverride = unreadOverride
        self.updatedAtHlc = updatedAtHlc
    }
}

public struct ReadCursorState: Equatable {
    public let upToMessageId: Data
    public let readAtMs: UInt64
    public let updatedAtHlc: UInt64

    public init(upToMessageId: Data, readAtMs: UInt64, updatedAtHlc: UInt64) {
        self.upToMessageId = upToMessageId
        self.readAtMs = readAtMs
        self.updatedAtHlc = updatedAtHlc
    }
}

public struct OrderState: Equatable {
    public let groupIds: [Data]
    public let updatedAtHlc: UInt64

    public init(groupIds: [Data], updatedAtHlc: UInt64) {
        self.groupIds = groupIds
        self.updatedAtHlc = updatedAtHlc
    }
}

/// Last-write-wins resolver. Returns `true` when `candidate`
/// should replace `existing` (higher HLC, or no existing
/// record), `false` when it should be dropped (older or
/// duplicate).
public func shouldApply(candidateHlc: UInt64, existingHlc: UInt64?) -> Bool {
    guard let existing = existingHlc else { return true }
    return candidateHlc > existing
}

/// Hex-encode a `group_id` byte slice for use as a map key.
/// Lowercase, no `0x` prefix. Same encoding the web side uses.
public func groupIdHex(_ bytes: Data) -> String {
    return bytes.map { String(format: "%02x", $0) }.joined()
}
