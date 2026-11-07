// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0087 phase B — the `/g/` links of the account (a group's, a
/// channel's subscriber links, a channel's publisher links) kept in the
/// synced link register (ADR-0079 section 4), keyed by
/// `groupLinkRecordId(ledgerId)`, so every device of the account lists them
/// with their labels and any of them revokes one; the revoking device deletes
/// the ledger row with the revocation seed the record carries.
@MainActor
enum GroupLinkRegister {
    /// The account's DeviceGroup sync runtime, set by the signed-in shell.
    static weak var sync: DeviceGroupSyncRepository?

    /// Record a link just minted. `false` when the register is not loaded or
    /// refused it (the link still works; it is listed on this device only).
    @discardableResult
    static func record(
        kind: LinkRecordKind, ledgerId: Data, linkSecret: Data, revocationSeed: Data, targetId: Data,
        inviteSeed: Data = Data(), admissionVersion: UInt16 = 0, capability: Data = Data(),
        label: String = "", expiryMs: UInt64 = 0, maxUses: UInt32 = UInt32.max
    ) async -> Bool {
        guard let sync, let linkId = try? groupLinkRecordId(ledgerId: ledgerId) else { return false }
        return await sync.setLinkRecord(LinkRecord(
            hlc: 0, linkId: linkId, linkSecret: linkSecret, kind: kind, ledgerId: ledgerId,
            revocationSeed: revocationSeed, expiryMs: expiryMs, maxUses: maxUses, uses: 0,
            revoked: false, mintedAtMs: UInt64(Date().timeIntervalSince1970 * 1000), label: label,
            answered: [], targetId: targetId, inviteSeed: inviteSeed, admissionVersion: admissionVersion,
            capability: capability))
    }

    /// The live links of `kinds` for `targetId`, newest first.
    static func records(kinds: Set<LinkRecordKind>, targetId: Data) -> [LinkRecord] {
        (sync?.linkRecords ?? [])
            .filter { kinds.contains($0.kind) && $0.targetId == targetId && !$0.revoked }
            .sorted { $0.mintedAtMs > $1.mintedAtMs }
    }

    /// The record of a ledger id, revoked or not.
    static func record(ledgerId: Data) -> LinkRecord? {
        sync?.linkRecords.first { $0.ledgerId == ledgerId && $0.kind != .contact && $0.kind != .call }
    }

    /// Mark a link revoked in the register (every device stops listing it).
    static func markRevoked(ledgerId: Data) async {
        guard let record = record(ledgerId: ledgerId) else { return }
        await sync?.revokeLinkRecord(linkId: record.linkId)
    }

    /// The link's URL, from the record.
    static func url(_ record: LinkRecord) -> String? {
        guard let base = try? GroupInviteUrl.composePointerGroupInviteUrl(
            blobId: record.ledgerId, origin: CoPublisher.defaultOrigin) else { return nil }
        return InviteLinkSecret.link(base: base, secret: record.linkSecret)
    }
}
