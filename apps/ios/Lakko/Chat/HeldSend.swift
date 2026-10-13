// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0073 R.7 case 1 / UX 16.4 - the HELD send of a one-to-one whose other
// person reset their keys.
//
// iOS mirror of `apps/web/src/chat/held-send.ts`; the tables are the same and
// so are the cases in `HeldSendTests`.
//
// Once the reset is accepted, a message is never sent into the old session
// (only removed devices would get it). Until the successor exists it is held:
// the bubble shows the ordinary sending state and the message goes out when
// the session exists; after the give-up it shows the ordinary failed state
// with its ordinary retry. No copy of its own (R.0).
//
// What is held: a text message, a media message (once its bytes are uploaded)
// and a forward into such a one-to-one. What is NOT held is an op on a message
// of the old session (an edit, a delete, a reaction): its target exists only
// in the old session, so there is nothing a successor could apply it to. It is
// applied on this device alone and never sent (`opRoute`); the founder's
// re-send of its own messages reads the stored text, which is the edited text,
// and skips a deleted one.
//
// The in-memory store is keyed on the CONVERSATION id, so a held message
// survives the view moving onto the successor group (the moment it can finally
// go out). `HeldSendQueue` keeps it across a relaunch.
//
// A group is never held: a member sends where its device is (R.7 case 3).
public enum HeldSend {

    /// The conversation's kind as these decisions read it (`.chat` is a
    /// one-to-one). Kept local so this module stands alone; the caller maps
    /// the app's own conversation kind onto it.
    public enum Kind: Equatable {
        case chat, group
    }

    // MARK: - sendRoute

    /// Where a composed message goes now.
    public enum SendRoute: Equatable {
        /// Into the group the view has open.
        case send
        /// Nowhere yet: the successor of this one-to-one does not exist here.
        case hold
    }

    /// | kind  | the open group is unrepairable for an accepted reset | route  |
    /// |-------|------------------------------------------------------|--------|
    /// | chat  | yes                                                  | `hold` |
    /// | chat  | no                                                   | `send` |
    /// | group | any                                                  | `send` |
    ///
    /// `unrepairableKeyB64` is the "unrepairable by" key over the OPEN group:
    /// once the view is on the successor (whose roster holds the new identity)
    /// it is `nil` and the message goes out.
    public static func sendRoute(kind: Kind, unrepairableKeyB64: String?) -> SendRoute {
        kind == .chat && unrepairableKeyB64 != nil ? .hold : .send
    }

    // MARK: - opRoute

    /// What an op on a message (edit, delete, reaction) does now.
    public enum OpRoute: Equatable {
        /// Publish it, then apply it locally.
        case send
        /// Apply it on this device only; nothing is published.
        case localOnly
    }

    /// | kind  | the open group is unrepairable for an accepted reset | op route    |
    /// |-------|------------------------------------------------------|-------------|
    /// | chat  | yes                                                  | `localOnly` |
    /// | chat  | no                                                   | `send`      |
    /// | group | any                                                  | `send`      |
    ///
    /// The same condition as `sendRoute`'s `hold`: where a message would be
    /// held, an op is local.
    public static func opRoute(kind: Kind, unrepairableKeyB64: String?) -> OpRoute {
        sendRoute(kind: kind, unrepairableKeyB64: unrepairableKeyB64) == .hold ? .localOnly : .send
    }

    // MARK: - flushVerdict

    /// Whether the background flushes a conversation's held messages now.
    public enum FlushVerdict: Equatable {
        /// Send them into the conversation's current group.
        case flush
        /// Leave them held.
        case wait
    }

    /// | held | a view of the conversation is mounted | head is live here | head route | verdict |
    /// |------|---------------------------------------|-------------------|------------|---------|
    /// | 0    | any                                   | any               | any        | `wait`  |
    /// | > 0  | yes (the view resumes them itself)    | any               | any        | `wait`  |
    /// | > 0  | no                                    | no                | any        | `wait`  |
    /// | > 0  | no                                    | yes               | `hold`     | `wait`  |
    /// | > 0  | no                                    | yes               | `send`     | `flush` |
    ///
    /// `headRoute` is `sendRoute` over the conversation's CURRENT group as this
    /// device holds it: while that is still the superseded one-to-one the
    /// route is `hold`; once the successor is the head it is `send`. There is
    /// no give-up here: a message nobody is looking at is not failed, it
    /// waits.
    public static func flushVerdict(
        held: Int, viewMounted: Bool, headActive: Bool, headRoute: SendRoute
    ) -> FlushVerdict {
        if held <= 0 || viewMounted || !headActive { return .wait }
        return headRoute == .send ? .flush : .wait
    }

    // MARK: - isDeadSession

    /// Whether a group is a DEAD SESSION: a one-to-one whose other person's
    /// reset this device accepted. The predicate of the publish guard: into
    /// such a group nothing is published but the successor claim.
    ///
    /// A GROUP conversation is never a dead session, whoever reset: its
    /// members send where their device is (R.7 cases 2 and 3).
    public static func isDeadSession(kind: Kind, unrepairableKeyB64: String?) -> Bool {
        sendRoute(kind: kind, unrepairableKeyB64: unrepairableKeyB64) == .hold
    }

    // MARK: - what a dead session does with each outgoing thing

    /// Every kind of thing a chat can try to put on the wire.
    public enum Outgoing: Equatable, CaseIterable {
        /// A typed message.
        case text
        /// A media message whose bytes are uploaded.
        case media
        /// A forward INTO the conversation.
        case forward
        /// Ops on a message of the session.
        case edit, delete, reaction
        /// Ephemeral signals.
        case typing, readReceipt
        /// The successor claim (R.7 case 1, claim-then-build).
        case successorClaim
    }

    /// What is done with an outgoing thing.
    public enum OutgoingAction: Equatable {
        /// Seal and publish it into the group.
        case publish
        /// Hold it for the successor (`HeldSend.Store`, `HeldSendQueue`).
        case hold
        /// Apply it on this device only; nothing is published.
        case localOnly
        /// Nothing is sent and nothing is applied.
        case notSent
    }

    /// | the group is a dead session | outgoing                  | action      |
    /// |-----------------------------|---------------------------|-------------|
    /// | no                          | any                       | `publish`   |
    /// | yes                         | text / media / forward    | `hold`      |
    /// | yes                         | edit / delete / reaction  | `localOnly` |
    /// | yes                         | typing / read receipt     | `notSent`   |
    /// | yes                         | successor claim           | `publish`   |
    ///
    /// The claim is the ONE thing published into a dead session: the removed
    /// identity's other-side devices that are still the person's read it
    /// there. Web expresses the same table as `sendRoute` + `opRoute` at the
    /// callers and a publish guard that refuses everything but the claim
    /// (`chat/client.ts` `installDeadSessionGuard`); here it is one function,
    /// so the guard and the callers cannot disagree.
    public static func outgoingAction(
        kind: Kind, unrepairableKeyB64: String?, outgoing: Outgoing
    ) -> OutgoingAction {
        guard isDeadSession(kind: kind, unrepairableKeyB64: unrepairableKeyB64) else { return .publish }
        switch outgoing {
        case .text, .media, .forward: return .hold
        case .edit, .delete, .reaction: return .localOnly
        case .typing, .readReceipt: return .notSent
        case .successorClaim: return .publish
        }
    }

    /// The publish guard's answer: whether a publish into the group is
    /// refused before anything is sealed.
    ///
    /// | the group is a dead session | the publish is the successor claim | refused |
    /// |-----------------------------|------------------------------------|---------|
    /// | no                          | any                                | no      |
    /// | yes                         | yes                                | no      |
    /// | yes                         | no                                 | yes     |
    public static func publishRefused(isDeadSession: Bool, isSuccessorClaim: Bool) -> Bool {
        isDeadSession && !isSuccessorClaim
    }

    /// Thrown by the publish path for a refused publish (web
    /// `HeldForSuccessor`): nothing was sealed or sent. A sender of a message
    /// holds it; every other caller treats it as "not sent".
    public struct HeldForSuccessor: Error, Equatable {
        public init() {}
    }

    // MARK: - heldVerdict

    /// How long a held message waits before it shows the ordinary failed
    /// state. Chosen, not measured: long enough for the other person's new
    /// device to publish what is needed to add it, short enough that
    /// "sending" does not sit on screen for an evening.
    public static let giveUpMs: Int64 = 10 * 60 * 1000

    /// What to do with a held message now.
    public enum HeldVerdict: Equatable {
        case send, wait, giveUp
    }

    /// | route  | held for             | verdict  |
    /// |--------|----------------------|----------|
    /// | `send` | any                  | `send`   |
    /// | `hold` | under the give-up    | `wait`   |
    /// | `hold` | the give-up or more  | `giveUp` |
    public static func heldVerdict(route: SendRoute, heldAtMs: Int64, nowMs: Int64) -> HeldVerdict {
        if route == .send { return .send }
        let (elapsed, overflow) = nowMs.subtractingReportingOverflow(heldAtMs)
        // An overflow can only come from a nonsense clock pair; it reads as
        // "the give-up passed" when now is the larger, "wait" otherwise.
        if overflow { return nowMs > heldAtMs ? .giveUp : .wait }
        return elapsed >= giveUpMs ? .giveUp : .wait
    }

    // MARK: - replyForSend

    /// The reply target a flushed reply carries when its original belongs to
    /// another segment: a DS message id no row has (ids start at 1, history
    /// rows are negative), so the quote is drawn from its snapshot and a tap
    /// on it finds nothing. The wire needs SOME target for the snapshot to be
    /// carried (the core drops a snapshot without one), so "no jump target"
    /// is this value, not an absent field. On receive, 0 means "no jump".
    public static let replyNoJumpTarget: Int64 = 0

    /// A reply reference as a held message carries it: the target message id
    /// in the journal it was composed in, and the quote snapshot.
    public struct Reply: Equatable, Codable {
        public let target: Int64
        public let author: String?
        public let excerpt: String?

        public init(target: Int64, author: String?, excerpt: String?) {
            self.target = target
            self.author = author
            self.excerpt = excerpt
        }
    }

    /// ADR-0073 R.7 case 1 - the reply reference a held message is SENT with.
    ///
    /// A reply names its original by that message's id in the journal it was
    /// composed in. A held reply is composed in the old one-to-one and flushed
    /// into the successor, whose ids restart: the old id there names nothing,
    /// or worse, an unrelated message. So the quote (author and excerpt, a
    /// snapshot taken at compose time) is kept and the jump target is dropped.
    ///
    /// | reply | composed in group       | sent into group | result                     |
    /// |-------|-------------------------|-----------------|----------------------------|
    /// | none  | -                       | -               | none                       |
    /// | set   | the same                | the same        | unchanged                  |
    /// | set   | another                 | -               | quote kept, target dropped |
    /// | set   | unknown (not recorded)  | -               | quote kept, target dropped |
    ///
    /// The last row is the fail-safe direction: a held message whose compose
    /// group was not recorded is never sent with a target that may be wrong.
    ///
    /// Generic over the caller's reply type: `target` reads its target and
    /// `withTarget` rebuilds it with another (everything else kept).
    public static func replyForSend<R>(
        _ reply: R?,
        composedInGroupIdB64: String?,
        sendIntoGroupIdB64: String,
        target: (R) -> Int64,
        withTarget: (R, Int64) -> R
    ) -> R? {
        guard let reply else { return nil }
        if let composedInGroupIdB64, composedInGroupIdB64 == sendIntoGroupIdB64 { return reply }
        return target(reply) == replyNoJumpTarget ? reply : withTarget(reply, replyNoJumpTarget)
    }

    /// `replyForSend` over this module's own `Reply`.
    public static func replyForSend(
        _ reply: Reply?, composedInGroupIdB64: String?, sendIntoGroupIdB64: String
    ) -> Reply? {
        replyForSend(
            reply,
            composedInGroupIdB64: composedInGroupIdB64,
            sendIntoGroupIdB64: sendIntoGroupIdB64,
            target: { $0.target },
            withTarget: { Reply(target: $1, author: $0.author, excerpt: $0.excerpt) })
    }

    // MARK: - flushedRowsToShow

    /// ADR-0073 R.7 case 1 - which of the messages a background flush just
    /// sent the open view still has to show.
    ///
    /// The background flushes a conversation's held messages only while no
    /// view of it is attached; a view that is APPEARING at that moment has
    /// already read the log (before the flush wrote to it) and finds the
    /// queue empty, so without this it would show the messages only after a
    /// later re-read. The flush reports what it sent; the view adds the rows
    /// it does not hold.
    ///
    /// | the view holds a row with that message id | shown now |
    /// |-------------------------------------------|-----------|
    /// | yes (the log read came after the write)   | no        |
    /// | no                                        | yes       |
    ///
    /// Order is the flush's (send order); a message id repeated in the report
    /// is shown once; a message whose id cannot be read (`nil`) is dropped.
    public static func flushedRowsToShow<M>(
        shownMessageIds: Set<Int64>, flushed: [M], messageId: (M) -> Int64?
    ) -> [M] {
        var seen = shownMessageIds
        var out: [M] = []
        for message in flushed {
            guard let id = messageId(message), !seen.contains(id) else { continue }
            seen.insert(id)
            out.append(message)
        }
        return out
    }

    // MARK: - the per-session store

    /// One held message. `payload` is the caller's own (text, attachments,
    /// reply).
    public struct Item<T> {
        public let heldAtMs: Int64
        public let payload: T

        public init(heldAtMs: Int64, payload: T) {
            self.heldAtMs = heldAtMs
            self.payload = payload
        }
    }

    /// The per-session store of held messages, by conversation id.
    /// Conversations are listed in the order they first held something.
    /// Not thread-safe: confine it to one actor (the main actor in the app).
    public final class Store<T> {
        private var order: [String] = []
        private var byConversation: [String: [Item<T>]] = [:]

        public init() {}

        private func set(_ conversationId: String, _ list: [Item<T>]) {
            if byConversation[conversationId] == nil { order.append(conversationId) }
            byConversation[conversationId] = list
        }

        /// Hold one more message of `conversationId`, in send order.
        public func hold(conversationId: String, payload: T, heldAtMs: Int64) {
            set(conversationId, (byConversation[conversationId] ?? []) + [Item(heldAtMs: heldAtMs, payload: payload)])
        }

        /// Take every held message of `conversationId`, in send order. The
        /// store no longer holds them: whoever took them owns sending them.
        public func take(conversationId: String) -> [Item<T>] {
            guard let list = byConversation.removeValue(forKey: conversationId) else { return [] }
            order.removeAll { $0 == conversationId }
            return list
        }

        /// Put messages back at the FRONT, in the order given (a flush that
        /// stopped part-way returns what it did not send).
        public func putBack(conversationId: String, items: [Item<T>]) {
            if items.isEmpty { return }
            set(conversationId, items + (byConversation[conversationId] ?? []))
        }

        /// Every conversation with something held, with its messages in order.
        public func all() -> [(conversationId: String, items: [Item<T>])] {
            order.map { (conversationId: $0, items: byConversation[$0] ?? []) }
        }

        /// Replace everything (a restore from disk at start).
        public func replaceAll(_ entries: [(conversationId: String, item: Item<T>)]) {
            order = []
            byConversation = [:]
            for entry in entries {
                set(entry.conversationId, (byConversation[entry.conversationId] ?? []) + [entry.item])
            }
        }

        /// How many are held for `conversationId`.
        public func count(conversationId: String) -> Int {
            byConversation[conversationId]?.count ?? 0
        }

        /// Sign-out: nothing held survives into the next account.
        public func clear() {
            order = []
            byConversation = [:]
        }
    }

    // MARK: - identityCheckDue

    /// How old the last identity check of a one-to-one's peer may be before a
    /// send (and an open) runs it again (R.2 "Triggers"): the re-admission
    /// arm's interval.
    public static func identityCheckDue(lastCheckedAtMs: Int64?, nowMs: Int64, intervalMs: Int64) -> Bool {
        guard let lastCheckedAtMs else { return true }
        let (elapsed, overflow) = nowMs.subtractingReportingOverflow(lastCheckedAtMs)
        if overflow { return nowMs > lastCheckedAtMs }
        return elapsed >= intervalMs
    }
}
