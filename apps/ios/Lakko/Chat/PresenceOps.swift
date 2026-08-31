// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0024 — the pure receive-side folds behind the typing indicator and the
/// "Sent" / "Read" check, lifted out of ``ChatViewModel`` so they can be tested
/// without a view model, a repository or a live MLS group.
///
/// iOS mirror of `apps/web/src/chat/presence-ops.ts` (which iOS never had — the
/// logic lived inline in the view model, which is why the leaf-keying below
/// went unnoticed for as long as it did).
///
/// ## Both registries are keyed on a signature key, never a leaf
///
/// OpenMLS hands a vacated leaf to the next member added, so a leaf-keyed
/// registry silently transfers the departed member's state to the newcomer. For
/// the read watermark — which is monotone and never reset on a membership
/// change — that meant the newcomer inherited the departed member's `readUpTo`
/// outright, and ``isReadByEveryOther(watermarks:messageId:roster:own:)`` then
/// reported messages as read by someone who never saw them. Typing has the same
/// shape with a ~6 s lifetime; it is keyed the same way so there is one rule to
/// remember rather than two. See ``AuthorIdentity``.
public enum PresenceOps {

    /// Re-sync replay guard + auto-expiry window. A typing signal whose DS
    /// `created_at` is older than this is ignored on receive (a full journal
    /// re-download would otherwise resurrect stale typing); a displayed
    /// "typing…" auto-clears this long after that member's last fresh `true`.
    public static let typingExpiry: TimeInterval = 6

    /// Throttle for the OUTGOING `{ typing: true }` — at most one per window
    /// while the user is actively editing.
    public static let typingSendThrottle: TimeInterval = 4

    /// The per-group typing registry: member identity → the local receive time
    /// of that member's last fresh `typing: true`.
    public typealias TypingMap = [MemberIdentity: Date]

    /// The per-group read-watermark registry: member identity → the highest
    /// messageId that member has reported reading (monotone).
    public typealias ReadWatermarkMap = [MemberIdentity: Int64]

    /// Fold one incoming `typing` signal into the registry, returning a NEW
    /// map.
    ///
    ///   - `isTyping == false` → clear that member immediately (an explicit
    ///     stop). A newcomer's stop therefore cannot clear a departed member's
    ///     entry, because the two are different keys.
    ///   - `isTyping == true` with a DS row older than ``typingExpiry`` → DROP
    ///     (the re-sync replay guard).
    ///   - otherwise → stamp `now`.
    ///
    /// An unparseable `rowDate` (`nil`) KEEPS the signal: the freshness check
    /// fails open because the receive-time expiry sweep still bounds display.
    public static func foldTyping(
        _ prev: TypingMap,
        sender: MemberIdentity,
        isTyping: Bool,
        rowDate: Date?,
        now: Date
    ) -> TypingMap {
        var next = prev
        guard isTyping else {
            next[sender] = nil
            return next
        }
        if let rowDate, now.timeIntervalSince(rowDate) > typingExpiry { return prev }
        next[sender] = now
        return next
    }

    /// The members currently shown as typing at `now`, sorted so the rendered
    /// list is stable. Pure read.
    public static func activeTypers(_ map: TypingMap, now: Date) -> [MemberIdentity] {
        map.filter { now.timeIntervalSince($0.value) <= typingExpiry }
            .keys
            .sorted()
    }

    /// Drop every entry past the expiry window, returning a NEW map. Drives the
    /// ~1 Hz sweep.
    public static func pruneTyping(_ map: TypingMap, now: Date) -> TypingMap {
        map.filter { now.timeIntervalSince($0.value) <= typingExpiry }
    }

    /// Advance the monotone read watermark for `sender`, returning a NEW map. A
    /// lower-or-equal `upTo` never lowers a higher one, so a re-sync replay of
    /// an older read-receipt row is idempotent (ADR-0024).
    ///
    /// `sender` is a signature key: the watermark is monotone and is never
    /// reset on a membership change, so keying it on a leaf handed a departed
    /// member's entire read history to the next member added.
    public static func foldReadWatermark(
        _ prev: ReadWatermarkMap,
        sender: MemberIdentity,
        upTo: Int64
    ) -> ReadWatermarkMap {
        guard upTo > (prev[sender] ?? -1) else { return prev }
        var next = prev
        next[sender] = upTo
        return next
    }

    /// Has `messageId` been read by EVERY other member?
    ///
    /// `roster` is the CURRENT membership, so a departed member's stale
    /// watermark is simply never consulted, and a newcomer — who has reported
    /// nothing — holds the answer at "Sent" until they genuinely catch up. An
    /// empty "others" set (a solo group) reads as `true`, matching the
    /// pre-existing behaviour.
    public static func isReadByEveryOther(
        watermarks: ReadWatermarkMap,
        messageId: Int64,
        roster: [MemberIdentity],
        own: MemberIdentity?
    ) -> Bool {
        let others = roster.filter { $0 != own }
        guard !others.isEmpty else { return true }
        for member in others where (watermarks[member] ?? -1) < messageId {
            return false
        }
        return true
    }
}
