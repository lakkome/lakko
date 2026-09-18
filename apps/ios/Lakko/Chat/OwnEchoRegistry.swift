// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The rows this device published itself, so its own poll can drop them instead
/// of handing its own ciphertext back to OpenMLS.
///
/// iOS mirror of `apps/android/.../chat/OwnEchoRegistry.kt`.
///
/// ## The defect this closes
///
/// The registry was a bare `Set<Int64>` of DS message ids with **no group in
/// the key**, consulted by ``ChatClient/applyFetchedMessages(identity:group:groupIdHex:raw:after:)``
/// for every row of every group. But a DS message id is assigned **per
/// journal**, and there is one journal per group — so id 7 exists independently
/// in every chat. Publishing anything in chat A therefore armed a trap for the
/// next row numbered the same in chat B: the poll dropped it as "our own echo",
/// advanced the cursor past it, and it was gone for good. Every journal starts
/// at 1 and the ids stay small, so across a handful of conversations the
/// collision is the common case rather than a corner one.
///
/// What was lost was not only a chat bubble. Every 1x1-eligible call-signaling
/// row, every reaction, read receipt and service envelope, and every membership
/// commit rides the same journal and is dropped at the same line.
///
/// It presented as a screen dependence because of WHO applies the rows. A chat
/// that is open has its own uninterrupted long poll and consumes its own echoes
/// within a second, so an entry rarely outlives the send that made it. Every
/// other chat is applied by the app-level sweep
/// (``ForegroundReceiveScheduler``), where every group's rows run through this
/// one registry — so an offer arriving in a chat the user was not looking at
/// was silently swallowed by an id left behind by a message they had sent
/// somewhere else. The call was fetched and dropped, not fetched late.
///
/// ## The rule
///
/// An echo is identified by the journal it is on AND its id on that journal.
/// The key is a pair, and nothing in this type can be called without both
/// halves — which is the point of it being a type rather than a `Set<Int64>`
/// behind a lock: a bare id no longer type-checks at any call site.
///
/// ## Why an entry is consumed rather than expiring
///
/// A published row comes back exactly once, so ``consume(groupIdHex:messageId:)``
/// removes it: leaving it would make the set grow for the life of the session
/// and widen the very window this closes. A registration whose echo never
/// arrives (the session ends first) is simply lost with the session, which is
/// the pre-existing behaviour and is harmless — an unrecognised own row fails to
/// decrypt and is dropped one row later with a log line.
///
/// The exception is the entry that can never be consumed because the journal it
/// names will never be polled again — the device left that group, or its
/// membership ended. Under the old id-only key such an entry was a permanent
/// trap for that number in **every other** chat; under the pair it can only trap
/// its own dead journal, but it is still dead weight, so
/// ``forget(groupIdHex:)`` drops a whole journal's entries at those two moments.
public final class OwnEchoRegistry {

    /// One published row: the journal it went to, and its id on that journal.
    private struct Key: Hashable {
        let groupIdHex: String
        let messageId: Int64
    }

    /// A serial queue rather than a bare `Set`, because sends and polls run on
    /// different `Task`s and both mutate this; `Set` is value-typed but needs
    /// an external lock for concurrent mutation.
    private let lockQueue = DispatchQueue(label: "me.lakko.chat.ownEcho")
    private var entries: Set<Key> = []

    public init() {}

    /// Remember a row this device published to `groupIdHex`'s journal.
    public func remember(groupIdHex: String, messageId: Int64) {
        lockQueue.sync { _ = entries.insert(Key(groupIdHex: groupIdHex, messageId: messageId)) }
    }

    /// `true` when this row is one this device published to THIS journal, which
    /// also consumes it. `false` for the same id on any other journal — the
    /// whole point of the pair.
    public func consume(groupIdHex: String, messageId: Int64) -> Bool {
        lockQueue.sync {
            entries.remove(Key(groupIdHex: groupIdHex, messageId: messageId)) != nil
        }
    }

    /// Drop every entry for one journal, for a group this device has left or
    /// whose membership has ended: nothing will ever poll that journal again,
    /// so nothing can ever consume them.
    public func forget(groupIdHex: String) {
        lockQueue.sync { entries = entries.filter { $0.groupIdHex != groupIdHex } }
    }

    /// How many registrations are outstanding. Diagnostics and tests only.
    public var count: Int { lockQueue.sync { entries.count } }

    /// Forget everything — a client swap (logout) starts clean.
    public func clear() {
        lockQueue.sync { entries.removeAll() }
    }
}
