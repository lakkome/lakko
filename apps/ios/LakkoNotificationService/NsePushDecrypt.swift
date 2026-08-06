// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Read-only THROWAWAY MLS decrypt for the iOS silent-push NSE path
/// (ADR-0041 Phase 3).
///
/// The iOS counterpart of the web `public/sw-push-decrypt.js` (commit
/// `148753b`) and Android `PushPreviewDecrypt.kt` (commit `ae9a087`). When a
/// mutable-alert APNs wake lands, `NotificationService.didReceive` builds the
/// real (shared-store / slim-FFI / DS) dependency object and calls
/// ``decryptPushPreview(_:_:)`` to turn the `(group_pseudonym, pointer)` wake
/// hint into the REAL decrypted text of the newest message, so the
/// notification body says what actually arrived instead of a content-free
/// "New message".
///
/// ## Why this file holds no FFI / UserNotifications types
///
/// Every capability it needs — the store reads, the MLS restore + decrypt,
/// the DS fetch — is INJECTED via ``NsePushDecryptDeps``. The file imports
/// only `Foundation`, so it compiles into BOTH the `LakkoNotificationService`
/// (NSE) target — which supplies the real deps over the slim
/// `LakkoCoreExtension` framework (`NseDecryptDeps.swift`) — AND the
/// `LakkoTests` unit-test target, which drives it with hand-rolled fakes and
/// no `LakkoCoreExtension.xcframework` linked. The production wiring that
/// touches the slim FFI + Keychain lives in `NseDecryptDeps.swift`; the pure,
/// FFI-free decrypt logic tested here is this file.
///
/// ## Load-bearing correctness: READ-ONLY THROWAWAY
///
/// This path MUST NOT corrupt the MLS ratchet state the foreground app relies
/// on. (The NSE is a SEPARATE process from the app — there is no shared
/// in-memory handle to corrupt — but it must still never write BACK the
/// advanced snapshot / cursor, or the foreground app, which restores from the
/// SAME last-persisted snapshot and re-processes from its OWN cursor, would
/// desync its ratchet — the G-34 nonce-reuse-on-crash class.) It:
///
///   1. reads the AccountIdentity seed + the last-persisted MLS snapshot
///      (both READ-ONLY),
///   2. restores a TEMPORARY, in-memory throwaway group from that snapshot (a
///      fresh `Identity` + `Group` — NEVER the app's handles),
///   3. fetches the DS journal after a READ-ONLY cursor and processes the
///      rows through the temp group ONLY to extract the newest message's
///      plaintext preview,
///   4. returns the preview and DISCARDS the temp group (``NseThrowawayGroup/close()``).
///
/// It NEVER persists the advanced snapshot, NEVER advances the app's DS
/// cursor, NEVER writes any MLS state. ``NsePushDecryptDeps`` deliberately
/// exposes NO write capability — the protocol simply cannot persist, which is
/// the structural enforcement of the read-only-throwaway discipline. The slim
/// `LakkoCoreExtension` FFI itself exposes no `snapshotStorage` / persist
/// method, so there is no write path to reach even by accident.
///
/// ## Fail-closed (security invariant #4)
///
/// A frame that will not decrypt / authenticate is DROPPED, never shown as a
/// plaintext guess (``NseThrowawayGroup/process(_:)`` returns `nil`). Any
/// missing input (no account match, no seed, no snapshot, non-member group,
/// DS failure, zero decryptable frames) returns `nil`, and the NSE shows the
/// generic "New message" notification.
///
/// ## Scope
///
/// GroupMessage wakes only (chat / edit / attachment-hint text). A Wakeup /
/// MailboxMessage wake (no `groupHex` / no `pointer`) returns `nil` here and
/// stays generic (a follow-up), mirroring the web + Android slices.

/// Max characters of decrypted text shown in the notification body.
private let previewMaxChars = 140

/// Length in hex chars of a 32-byte group_id.
private let groupHexLen = 64

/// The GroupMessage wake hint, already decoded from the APNs `userInfo`
/// payload (`{ aps, d, g, p }`). Only `g` (group hex) + `p` (DS pointer) are
/// load-bearing for the preview.
public struct NsePushWake: Equatable {
    /// 64 lowercase-hex chars = the 32-byte group_id, or `nil`.
    public let groupHex: String?
    /// DS message id of the message that triggered the wake, or `nil`.
    public let pointer: Int64?

    public init(groupHex: String?, pointer: Int64?) {
        self.groupHex = groupHex
        self.pointer = pointer
    }
}

/// A locally-stored account that owns the wake's group.
public struct NseResolvedAccount: Equatable {
    /// The account key shared by every per-account store (the same
    /// standard-base64 string `KnownGroupsStore` / `AccountIdentitySeedStore`
    /// / `IdentitySnapshotStore` / `MessageLogStore` all key on).
    public let accountIdB64: String
    /// The raw 32-byte group_id (hex-decoded from the wake's `g`).
    public let groupIdBytes: Data

    public init(accountIdB64: String, groupIdBytes: Data) {
        self.accountIdB64 = accountIdB64
        self.groupIdBytes = groupIdBytes
    }
}

/// One DS journal row (server id + raw MLS wire bytes).
public struct NseFetchedRow: Equatable {
    public let id: Int64
    public let payload: Data

    public init(id: Int64, payload: Data) {
        self.id = id
        self.payload = payload
    }
}

/// Classification of a decrypted application message (mirror of the slim FFI
/// `MessageBodyKind`), kept FFI-free so it can be produced by the fake in the
/// unit test.
public enum NseProcessedKind: Equatable {
    case chat, edit, delete, reaction, typing, readReceipt
}

/// A decoded application row (post MLS-decrypt + envelope-parse).
public struct NseProcessedRow: Equatable {
    public let kind: NseProcessedKind
    /// Chat / edit text (empty for the other kinds).
    public let text: String
    /// Number of attachment refs (chat only).
    public let attachmentCount: Int

    public init(kind: NseProcessedKind, text: String, attachmentCount: Int) {
        self.kind = kind
        self.text = text
        self.attachmentCount = attachmentCount
    }
}

/// The notification preview the NSE renders when the decrypt succeeds.
public struct NsePushPreview: Equatable {
    public let title: String
    public let body: String
    /// The group hex — collapses a burst into one grouped notification.
    public let tag: String

    public init(title: String, body: String, tag: String) {
        self.title = title
        self.body = body
        self.tag = tag
    }
}

/// A TEMPORARY, in-memory MLS group restored from the last-persisted
/// snapshot. The production implementation (`NseRealThrowawayGroup`) wraps a
/// throwaway `Identity` + `Group`; ``close()`` frees them and MUST NEVER
/// persist / snapshot them back (read-only-throwaway discipline).
public protocol NseThrowawayGroup: AnyObject {
    /// Process ONE wire row through the throwaway group. Returns the decoded
    /// row, or `nil` when the frame does not decrypt / authenticate, is our
    /// own echo, or is a control frame (fail-closed skip — never a plaintext
    /// guess). MUST NOT persist any MLS state.
    func process(_ payload: Data) -> NseProcessedRow?

    /// Discard the throwaway handles. MUST NOT persist / snapshot.
    func close()
}

/// READ-ONLY injected capabilities. Every method is a read or a pure compute
/// — there is deliberately NO write path (no snapshot persist, no cursor
/// advance). The protocol simply cannot persist, which is the structural
/// enforcement of the read-only-throwaway discipline.
public protocol NsePushDecryptDeps {
    /// Match `groupHex` to a locally-stored account+group. `nil` when no
    /// account on this device knows the group (a stray wake).
    func resolveAccount(groupHex: String) -> NseResolvedAccount?

    /// The 64-byte AccountIdentity (BIP39) seed for `accountIdB64`, or `nil`.
    func loadIdentitySeed(accountIdB64: String) -> Data?

    /// The last-persisted MLS provider snapshot for `accountIdB64`, or `nil`.
    /// Only ever READ — never written back.
    func loadSnapshot(accountIdB64: String) -> Data?

    /// Build a THROWAWAY `Identity` + `Group` from the snapshot. `nil` when
    /// the account is not a member of the group / the restore fails.
    func openThrowawayGroup(
        seed: Data,
        snapshot: Data,
        groupIdBytes: Data
    ) throws -> NseThrowawayGroup?

    /// READ-ONLY DS group-journal fetch of rows strictly after `afterId`
    /// (ascending id order).
    func dsFetch(groupIdBytes: Data, afterId: Int64) async throws -> [NseFetchedRow]

    /// The app's last-persisted per-group cursor (READ-ONLY), or `nil`. Used
    /// as the fetch start so intervening commits replay and the epoch stays
    /// correct.
    func startCursor(accountIdB64: String, groupHex: String) -> Int64?
}

/// Produce a real decrypted notification preview for a GroupMessage wake, or
/// `nil` when the background path cannot (safely) yield one — in which case
/// the NSE shows its generic "New message" fallback.
///
/// Never throws for a "cannot decrypt" reason; every such path returns `nil`.
/// (A programming error in a mock could still throw in tests — the NSE wraps
/// the whole call in a `Task` + budget guard regardless.)
public func decryptPushPreview(
    _ wake: NsePushWake?,
    _ deps: NsePushDecryptDeps
) async -> NsePushPreview? {
    // 1. Only GroupMessage wakes carry a 64-hex group + a pointer. A Wakeup /
    //    MailboxMessage wake is not handled in this slice.
    guard let rawGroupHex = wake?.groupHex, let pointer = wake?.pointer else {
        return nil
    }
    let groupHex = rawGroupHex.lowercased()
    guard groupHex.count == groupHexLen, isLowerHex(groupHex) else { return nil }

    // 2. Which locally-stored account owns this group? (Also validates the
    //    wake's group is one we actually know — a stray wake → nil.)
    guard let account = deps.resolveAccount(groupHex: groupHex) else { return nil }

    // 3. The AccountIdentity seed → the throwaway Identity.
    guard var seed = deps.loadIdentitySeed(accountIdB64: account.accountIdB64) else {
        return nil
    }

    // 4. The last-persisted MLS provider snapshot (the ratchet state the
    //    foreground app relies on — we only READ it).
    guard let snapshot = deps.loadSnapshot(accountIdB64: account.accountIdB64) else {
        // Zero our seed view before bailing (the identity was never built).
        seed.resetBytes(in: 0..<seed.count)
        return nil
    }

    // 5. Build a TEMPORARY, in-memory group from the snapshot. Everything from
    //    here is throwaway — never snapshotted back.
    let opened: NseThrowawayGroup?
    do {
        opened = try deps.openThrowawayGroup(
            seed: seed,
            snapshot: snapshot,
            groupIdBytes: account.groupIdBytes
        )
    } catch {
        opened = nil
    }
    // The throwaway Identity has copied the seed; zero our view of it
    // regardless of success (mirrors the seed-store caller contract).
    seed.resetBytes(in: 0..<seed.count)
    guard let group = opened else { return nil }

    defer {
        // Discard the throwaway MLS state. We NEVER snapshot it back — doing
        // so would desync the foreground app's ratchet.
        group.close()
    }

    // 6. Fetch DS rows after a READ-ONLY start cursor. Prefer the app's
    //    persisted per-group cursor (so intervening commits since the
    //    snapshot are replayed and the epoch stays correct); otherwise just
    //    the message that triggered the wake (`pointer - 1`).
    var after = pointer - 1
    if let persisted = deps.startCursor(accountIdB64: account.accountIdB64, groupHex: groupHex),
       persisted < pointer {
        after = persisted
    }
    if after < 0 { after = 0 }

    let rows: [NseFetchedRow]
    do {
        rows = try await deps.dsFetch(groupIdBytes: account.groupIdBytes, afterId: after)
    } catch {
        return nil
    }

    // 7. Process each row through the TEMP group and keep the NEWEST frame
    //    that yields a user-visible preview. A frame that will not decrypt /
    //    authenticate (wrong epoch, our own echo, a control frame, a
    //    role-refused commit) is skipped — never guessed. NOTE: `process`
    //    mutates the THROWAWAY provider ONLY; it is never persisted.
    var latest: NsePushPreview?
    for row in rows {
        guard let processed = group.process(row.payload) else { continue }
        guard let body = previewBody(of: processed) else { continue }
        latest = NsePushPreview(title: "Lakko", body: body, tag: groupHex)
    }
    return latest
}

/// Extract a user-visible preview string from a processed row, or `nil` for
/// anything that is not a renderable chat/edit body (so an older real message
/// stays the preview).
private func previewBody(of row: NseProcessedRow) -> String? {
    switch row.kind {
    // A normal chat message → its text (or an attachment hint).
    case .chat:
        if !row.text.isEmpty { return truncate(row.text) }
        if row.attachmentCount == 1 { return "Sent an attachment" }
        if row.attachmentCount > 1 { return "Sent \(row.attachmentCount) attachments" }
        return nil
    // An edit rewrites a prior message → surface the new text.
    case .edit:
        return row.text.isEmpty ? nil : truncate(row.text)
    // delete / reaction / typing / read-receipt carry no standalone preview —
    // skip so an older real message stays the preview.
    case .delete, .reaction, .typing, .readReceipt:
        return nil
    }
}

/// Truncate a preview body to a sane notification length.
private func truncate(_ text: String) -> String {
    if text.count <= previewMaxChars { return text }
    return String(text.prefix(previewMaxChars - 1)) + "…"
}

/// `true` when every char of `s` is a lowercase hex digit.
private func isLowerHex(_ s: String) -> Bool {
    for c in s.unicodeScalars {
        let isDigit = c >= "0" && c <= "9"
        let isLower = c >= "a" && c <= "f"
        if !isDigit && !isLower { return false }
    }
    return true
}
