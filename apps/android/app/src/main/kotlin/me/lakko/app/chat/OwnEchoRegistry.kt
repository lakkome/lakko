// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.util.Collections

/**
 * The rows this device published itself, so its own poll can drop them instead
 * of handing its own ciphertext back to OpenMLS.
 *
 * ## The defect this closes
 *
 * An incoming call rang only while its own chat was the visible screen.
 *
 * The registry was a bare `MutableSet<Long>` of DS message ids with **no group
 * in the key**, consulted by `ChatClient.applyFetchedMessages` for every row of
 * every group. But a DS message id is assigned **per journal**, and there is one
 * journal per group — so id 7 exists independently in every chat. Publishing
 * anything in chat A therefore armed a trap for the next row numbered the same
 * in chat B: the poll dropped it as "our own echo", advanced the cursor past it,
 * and it was gone for good. Every chat starts at 1 and the ids stay small, so
 * across a handful of conversations the collisions are not a corner case.
 *
 * It presented as a screen dependence because of WHO applies the rows. A chat
 * that is open has its own uninterrupted poll and consumes its own echoes within
 * a second, so an entry rarely outlives the send that made it. Every other chat
 * is applied by the app-level all-groups poll, where every group's rows run
 * through this one set — so an offer arriving in a chat the user was not looking
 * at was silently swallowed by an id left behind by a message they had sent
 * somewhere else. The call was fetched and dropped, not fetched late.
 *
 * ## The rule
 *
 * An echo is identified by the journal it is on AND its id on that journal. The
 * key is a pair, and nothing in this file can be called without both halves.
 *
 * ## Why an entry is consumed rather than expiring
 *
 * A published row comes back exactly once, so [consume] removes it: leaving it
 * would make the set grow for the life of the session and widen the very window
 * this closes. A registration whose echo never arrives (the session ends first)
 * is simply lost with the session, which is the pre-existing behaviour and is
 * harmless — an unrecognised own row fails to decrypt and is dropped one row
 * later with a log line.
 */
public class OwnEchoRegistry {

    /**
     * `Collections.synchronizedSet` because sends and polls run on different
     * coroutines on different dispatchers and both mutate this.
     */
    private val entries: MutableSet<Key> = Collections.synchronizedSet(mutableSetOf())

    /** One published row: the journal it went to, and its id on that journal. */
    private data class Key(val groupIdHex: String, val messageId: Long)

    /** Remember a row this device published to [groupIdHex]'s journal. */
    public fun remember(groupIdHex: String, messageId: Long) {
        entries.add(Key(groupIdHex, messageId))
    }

    /**
     * `true` when this row is one this device published to THIS journal, which
     * also consumes it. `false` for the same id on any other journal — the whole
     * point of the pair.
     */
    public fun consume(groupIdHex: String, messageId: Long): Boolean =
        entries.remove(Key(groupIdHex, messageId))

    /** How many registrations are outstanding. Diagnostics and tests only. */
    public val size: Int get() = entries.size

    /** Forget everything — a client swap (logout) starts clean. */
    public fun clear(): Unit = entries.clear()
}
