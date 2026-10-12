// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0065 "Phase 2 revised", R.5 - what the views of a conversation do once
// it spans several groups (`Lineage.swift`): the pinned bar, the jump of a
// forward, the delete of a whole conversation and the feed's source list.
//
// iOS mirror of the lineage-related pure functions of
// `apps/web/src/chat/pin-bar.ts`, `chat/forward.ts`, `chat/delete-chat.ts`
// and `feed/feed-model.ts`; the tables are the same and so are the cases in
// `LineageViewsTests`.
//
// The inputs are plain values (hex / base64 ids, `Int64` message ids), not
// the app's own pin, forward or feed types, so every decision here can be
// run by a test without a store, a view or the core.
public enum LineageViews {

    // MARK: - Pins across segments

    /// The pins of ONE segment (group) of a conversation. `SharedPin` is
    /// whatever the shared pin map stores per message (a pin or a stored
    /// unpin with its stamp); it is carried through untouched.
    public struct SegmentPins<SharedPin> {
        /// How many segments this one is before the conversation's current
        /// group (`0` = the current group).
        public let distanceFromHead: Int
        /// `message id -> the stored shared pin`, in this group.
        public let shared: [Int64: SharedPin]
        /// `message id -> the time it was pinned for this person`, UNIX ms.
        public let personal: [Int64: UInt64]

        public init(distanceFromHead: Int, shared: [Int64: SharedPin], personal: [Int64: UInt64]) {
            self.distanceFromHead = distanceFromHead
            self.shared = shared
            self.personal = personal
        }
    }

    /// A conversation's pins, keyed on the ON-SCREEN row id.
    public struct SpannedPins<SharedPin> {
        public let shared: [Int64: SharedPin]
        public let personal: [Int64: UInt64]

        public init(shared: [Int64: SharedPin], personal: [Int64: UInt64]) {
            self.shared = shared
            self.personal = personal
        }
    }

    /// A conversation's pins span its segments. The pin store stays keyed on
    /// `(group, message id)` and is never rewritten when a conversation
    /// continues in a successor group, so a pin of an older segment is still
    /// held under the older group. This folds every held segment's pins into
    /// ONE pair of maps keyed on the ON-SCREEN row id:
    ///
    /// | the segment is ...            | its pin of message `m` is keyed on |
    /// |-------------------------------|------------------------------------|
    /// | the current group (`d` = 0)   | `m`                                |
    /// | `d` segments before the head  | `rowId(m, d)`                      |
    ///
    /// `rowId` is `LineageRules.historyRowId`, the id the timeline draws
    /// that row under and the id a tap jumps to. So a pin of an older
    /// segment stays in the bar and opens where it was. A negative distance
    /// is read as 0; of two segments that give one key the later in
    /// `segments` stands.
    ///
    /// Pure; the bar then orders the result exactly as before.
    public static func pinsAcrossSegments<SharedPin>(
        _ segments: [SegmentPins<SharedPin>],
        rowId: (Int64, Int) -> Int64 = { LineageRules.historyRowId($0, distanceFromHead: $1) }
    ) -> SpannedPins<SharedPin> {
        var shared: [Int64: SharedPin] = [:]
        var personal: [Int64: UInt64] = [:]
        for segment in segments {
            let d = max(0, segment.distanceFromHead)
            for (id, pin) in segment.shared { shared[d == 0 ? id : rowId(id, d)] = pin }
            for (id, at) in segment.personal { personal[d == 0 ? id : rowId(id, d)] = at }
        }
        return SpannedPins(shared: shared, personal: personal)
    }

    /// The stored pin an on-screen row id stands for.
    public struct PinTarget: Equatable {
        /// The group the pin is stored under (standard base64).
        public let groupIdB64: String
        /// The message id in THAT group.
        public let messageId: Int64
        /// Whether that group is an older segment of the conversation.
        public let older: Bool

        public init(groupIdB64: String, messageId: Int64, older: Bool) {
            self.groupIdB64 = groupIdB64
            self.messageId = messageId
            self.older = older
        }
    }

    /// The inverse of `pinsAcrossSegments`: which stored `(group, message
    /// id)` an on-screen row id is. A pin of an older segment is drawn under
    /// its history id; to UNPIN it the store has to be told the older group
    /// and the original id.
    ///
    /// | the row id is ...                                   | the pin is                         |
    /// |-----------------------------------------------------|------------------------------------|
    /// | not negative (a row of the current group)           | `(head, rowId)`, not older         |
    /// | a history id of a held older segment                | `(that group, original id)`, older |
    /// | a history id of a segment this device does not hold | `nil`                              |
    /// | negative, but no history id                         | `nil`                              |
    ///
    /// `olderGroupIdsB64` is the conversation's older segments, oldest first
    /// (distance `count - index` from the head), exactly as the bar folds
    /// them. `origin` is `LineageRules.historyRowOrigin`.
    public static func pinTargetOfRow(
        rowId: Int64,
        headGroupIdB64: String,
        olderGroupIdsB64: [String],
        origin: (Int64) -> LineageRules.HistoryRowOrigin? = { LineageRules.historyRowOrigin($0) }
    ) -> PinTarget? {
        if rowId >= 0 {
            return PinTarget(groupIdB64: headGroupIdB64, messageId: rowId, older: false)
        }
        guard let from = origin(rowId) else { return nil }
        let index = olderGroupIdsB64.count - from.distanceFromHead
        guard olderGroupIdsB64.indices.contains(index) else { return nil }
        return PinTarget(groupIdB64: olderGroupIdsB64[index], messageId: from.messageId, older: true)
    }

    /// Which scope a pin belongs to.
    public enum PinScope: Equatable {
        case shared, personal
    }

    /// How an unpin (or a pin) of one scope is carried out.
    public enum PinRoute: Equatable {
        /// The ordinary path: a shared change is sent to the group.
        case ordinary
        /// Applied on this device only: nothing is published.
        case localOnly
        /// Not offered.
        case refused
    }

    /// A pin of an OLDER segment can be removed, and nothing else can be
    /// done to it: an older segment is read-only on the wire (no op can name
    /// its rows in the current group's journal, and nothing is sent into a
    /// superseded session), so its shared unpin is recorded on this device
    /// alone, with a fresh stamp so a late re-send of the old pin cannot
    /// bring it back.
    ///
    /// | segment | change | scope    | route       |
    /// |---------|--------|----------|-------------|
    /// | current | any    | any      | `ordinary`  |
    /// | older   | unpin  | personal | `ordinary`  |
    /// | older   | unpin  | shared   | `localOnly` |
    /// | older   | pin    | any      | `refused`   |
    ///
    /// `pinned` is the state the change asks for (`true` = pin).
    public static func pinRoute(older: Bool, pinned: Bool, scope: PinScope) -> PinRoute {
        if !older { return .ordinary }
        if pinned { return .refused }
        return scope == .shared ? .localOnly : .ordinary
    }

    /// The pin row a message's action menu would carry.
    public enum PinRow: Equatable {
        case pin, unpin
    }

    /// What the action menu of a row of an OLDER segment offers: the `Unpin`
    /// row alone when the row is pinned in a way this device may clear, and
    /// nothing otherwise (a history row takes no edit, delete, reaction,
    /// reply, forward or new pin).
    ///
    /// | the row is ...            | its pin row | offers Unpin |
    /// |---------------------------|-------------|--------------|
    /// | of an older segment       | `unpin`     | yes          |
    /// | of an older segment       | `pin`       | no           |
    /// | of the current group      | any         | no (not this rule's) |
    public static func historyRowOffersUnpin(isHistoryRow: Bool, pinRow: PinRow) -> Bool {
        isHistoryRow && pinRow == .unpin
    }

    // MARK: - The jump of a forward

    /// The origin of a forward names a GROUP and a message id; the
    /// conversation that group belongs to may since have continued in a
    /// successor group. The jump then lands in an OLDER segment of the
    /// conversation, whose rows the timeline draws under `rowId(messageId,
    /// distance)` (`LineageRules.historyRowId`). This resolves the id of the
    /// row to jump to:
    ///
    /// | the origin group is ...                              | row to jump to            |
    /// |------------------------------------------------------|---------------------------|
    /// | the conversation's current group                     | the message id            |
    /// | an older segment this device holds (`d` before head) | `rowId(messageId, d)`     |
    /// | anything else (a segment not held, beyond the head)  | `nil`: nothing, silently  |
    ///
    /// `heldHistory` is the older segments this device holds, oldest first,
    /// as the view lists them; `d` counts within THAT list, which is what
    /// the timeline numbers its history rows by.
    public static func forwardJumpRow(
        originGroupIdB64: String,
        messageId: Int64,
        headGroupIdB64: String,
        heldHistory: [String],
        rowId: (Int64, Int) -> Int64 = { LineageRules.historyRowId($0, distanceFromHead: $1) }
    ) -> Int64? {
        if originGroupIdB64 == headGroupIdB64 { return messageId }
        guard let index = heldHistory.firstIndex(of: originGroupIdB64) else { return nil }
        return rowId(messageId, heldHistory.count - index)
    }

    // MARK: - Deleting a whole conversation

    /// One group of a conversation, as the delete reads it.
    public struct ConversationSegment: Equatable {
        /// Standard base64 of the group id: the key of every per-group store.
        public let groupIdB64: String
        /// Hex of the same id: the key the message log is filed under.
        public let groupIdHex: String

        public init(groupIdB64: String, groupIdHex: String) {
            self.groupIdB64 = groupIdB64
            self.groupIdHex = groupIdHex
        }
    }

    /// The order in which a conversation's groups are deleted: every held
    /// segment that is not the head first (in the order given), the HEAD
    /// last. The head's row is what keeps the conversation's row on screen,
    /// so a failure at any earlier segment leaves the conversation
    /// reachable, exactly as the delete of one group does.
    ///
    /// | `held` contains ...           | the order is                         |
    /// |-------------------------------|--------------------------------------|
    /// | the head and other segments   | the others as given, then the head   |
    /// | a segment named twice         | that segment once, at its first place |
    /// | no head (not held here)       | whatever is held, as given           |
    public static func conversationDeleteOrder(
        _ held: [ConversationSegment],
        headGroupIdB64: String
    ) -> [ConversationSegment] {
        var seen: Set<String> = []
        var rest: [ConversationSegment] = []
        var head: ConversationSegment?
        for segment in held {
            if !seen.insert(segment.groupIdB64).inserted { continue }
            if segment.groupIdB64 == headGroupIdB64 {
                head = segment
            } else {
                rest.append(segment)
            }
        }
        guard let head else { return rest }
        return rest + [head]
    }

    // MARK: - The feed's sources

    /// What a feed source is.
    public enum FeedSourceKind: Equatable {
        case group, channel
    }

    /// One group or channel the feed may take items from.
    public struct FeedSource: Equatable {
        public let kind: FeedSourceKind
        /// The log key: the message-log key of a group, the hex id of a
        /// channel.
        public let idHex: String
        /// The CONVERSATION a group source belongs to (the hex of its
        /// lineage root), when that differs from `idHex`: a conversation
        /// that continued in a successor group is still ONE source, drawn
        /// from its current group (`idHex`) and named, muted, hidden and
        /// listed in feed rules under the conversation id. `nil` = `idHex`
        /// (a conversation that never moved, and every channel).
        public let conversationHex: String?
        /// What opens it.
        public let slug: String
        /// What the conversation list calls it.
        public let title: String
        /// Muted in this account's conversation flags.
        public let muted: Bool
        /// People in a group, for weighing reactions against its size; 0
        /// for a channel.
        public let audience: Int

        public init(
            kind: FeedSourceKind,
            idHex: String,
            conversationHex: String? = nil,
            slug: String,
            title: String,
            muted: Bool,
            audience: Int
        ) {
            self.kind = kind
            self.idHex = idHex
            self.conversationHex = conversationHex
            self.slug = slug
            self.title = title
            self.muted = muted
            self.audience = audience
        }
    }

    /// The key of everything a PERSON attaches to a feed source: the mute
    /// flag, "hide from feeds", a feed rule's include / exclude lists. The
    /// conversation id, never the group a moved conversation currently lives
    /// in, so none of it is lost or duplicated when the conversation moves.
    public static func feedSourceKey(idHex: String, conversationHex: String?) -> String {
        conversationHex ?? idHex
    }

    /// `feedSourceKey(idHex:conversationHex:)` of a source.
    public static func feedSourceKey(_ source: FeedSource) -> String {
        feedSourceKey(idHex: source.idHex, conversationHex: source.conversationHex)
    }

    /// One group conversation as the feed's source list reads it.
    public struct GroupConversationForFeed: Equatable {
        /// Hex of the conversation id (the lineage root).
        public let conversationHex: String
        /// Hex of the group it currently lives in (the lineage head).
        public let headHex: String
        public let slug: String
        public let title: String
        public let audience: Int

        public init(conversationHex: String, headHex: String, slug: String, title: String, audience: Int) {
            self.conversationHex = conversationHex
            self.headHex = headHex
            self.slug = slug
            self.title = title
            self.audience = audience
        }
    }

    /// The group sources of the feed: ONE per conversation, drawn from its
    /// current group, with the mute flag read under the conversation id.
    ///
    /// | the conversation ...          | its source                                             |
    /// |-------------------------------|--------------------------------------------------------|
    /// | never moved (head = root)     | `idHex` = the group, `conversationHex` = `nil`         |
    /// | moved to a successor          | `idHex` = the head, `conversationHex` = the root       |
    /// | is listed a second time       | left out: the first entry stands                       |
    public static func groupFeedSources(
        _ conversations: [GroupConversationForFeed],
        mutedOf: (String) -> Bool
    ) -> [FeedSource] {
        var seen: Set<String> = []
        var out: [FeedSource] = []
        for c in conversations {
            if !seen.insert(c.conversationHex).inserted { continue }
            out.append(
                FeedSource(
                    kind: .group,
                    idHex: c.headHex,
                    conversationHex: c.conversationHex == c.headHex ? nil : c.conversationHex,
                    slug: c.slug,
                    title: c.title,
                    muted: mutedOf(c.conversationHex),
                    audience: c.audience
                ))
        }
        return out
    }
}
