// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import uniffi.lakko_ffi_uniffi.Group
import uniffi.lakko_ffi_uniffi.GroupMember

/**
 * Who did this — the resolution rule behind the ADR-0023 author-only
 * edit/delete gate, the ADR-0023 reaction attribution, and the ADR-0024 typing
 * + read-watermark registries, plus the four folds that key on it.
 *
 * ## Why a signature key and not a leaf index
 *
 * An MLS leaf index is a **position in the ratchet tree, not a person**.
 * OpenMLS hands a vacated leaf to the next member added, so `remove Bob, add
 * Dave` — which ADR-0045 composes into ONE commit, landing in a single poll
 * batch — leaves Dave holding Bob's index. Every rule that compares raw leaf
 * indices then reads Dave as Bob: Dave may edit and delete Bob's history,
 * destroy Bob's reactions, and inherit Bob's read watermark. On Android the
 * author leaf was also PERSISTED, so the mis-binding survived restarts for the
 * whole retained transcript.
 *
 * The Ed25519 **signature public key** is the stable identity: it names the
 * member, it survives a renumber, and it is what every rule written after
 * ADR-0023 already keys on for exactly this reason — the role table
 * (`group_roles.rs`), the committer rank, the ADR-0045 beacon observation
 * store, the ADR-0042 push-routing authority gate, the ADR-0043 membership
 * shrink diff. ADR-0023 predates that convention; this module brings it in
 * line. Note an MLS `self_update` does NOT rotate the signature key
 * (`crates/lakko-mls/src/group.rs` passes `LeafNodeParameters::default()`,
 * keeping the same keys), so the identity is stable across an update too.
 *
 * ## Why resolution happens at receipt
 *
 * A leaf only names a member **in the epoch the message was sealed at**.
 * `processIncomingVerifyingRoles` returning an Application frame is the one
 * moment the group's roster IS that epoch's roster — an Application message
 * only decrypts at the current epoch, and a Commit that renumbers leaves is a
 * separate journal row applied before or after, never during. So the leaf is
 * resolved to a key THERE, inside the receive loop, and the KEY is what travels
 * onward and gets persisted — never the leaf, resolved later against whatever
 * roster happens to be current then. Resolving late is not a smaller version of
 * the same fix; it IS the impersonation, and
 * `AuthorIdentityTest.a leaf resolved LATE authorises the impersonation` keeps
 * a permanent contrast proving it.
 *
 * That "resolve now, carry the key" shape is the one `CommitterBeacon.kt` and
 * `GroupPushRouting` already use for the same reason.
 */

/**
 * A member's stable identity: their Ed25519 signature public key, standard
 * base64 — the same encoding [outboxB64], [RosterEntryView.signatureKeyB64] and
 * the accepted-contact map use, so one member has one string everywhere in the
 * client.
 *
 * A `String` rather than the raw `ByteArray` deliberately: Kotlin `ByteArray`
 * equality is REFERENCE equality, so a byte-array map key or `==` comparison
 * would silently never match and every gate would fail closed — a bug that
 * looks like "edit does nothing" rather than like a type error.
 */
public typealias MemberIdentity = String

/**
 * Sentinel identity for an OWN reaction whose signature key this device could
 * not resolve (a torn roster read). It still toggles and still counts, which is
 * what the local chip needs — the envelope has already been published, so
 * dropping the local apply would leave the peers showing a reaction the sender
 * cannot see or undo.
 *
 * `?` is outside the base64 alphabet, so this can never collide with a real
 * key — the property the old numeric `-1`-style sentinel had against a real
 * leaf index, carried over deliberately rather than re-derived.
 */
internal const val OWN_REACTION_SENTINEL: MemberIdentity = "?own"

/**
 * ADR-0024 — a typing indicator auto-expires this long after the last fresh
 * `typing:true`; the same window is the stale-`createdAt` re-sync replay guard.
 * ~6 s per the ADR (the sender re-emits every ~4 s while typing, so a live
 * typist always refreshes inside this).
 *
 * Top-level here rather than on `GroupRecord`'s companion, where it used to
 * live: [foldTyping] and [pruneTyping] are the rules it bounds, and
 * `GroupRecord` is a private class, so a fold that reads it cannot be reached
 * from a test while it stays there.
 */
internal const val TYPING_EXPIRY_MS: Long = 6_000L

/**
 * Resolve one MLS leaf index to the member's signature key, against the roster
 * the caller is holding — which must be the roster of the epoch the message
 * decrypted at. See the file doc on why the caller, not this function, owns
 * that timing.
 *
 * `null` on a miss — a `null` leaf (a control frame / external sender), an
 * unknown leaf, or a roster the caller could not read. Fail-closed is the whole
 * contract: every consumer treats an unresolved identity the way it treats an
 * unknown author, which is to refuse.
 */
internal fun resolveMemberIdentity(
    roster: List<GroupMember>,
    leafIndex: UInt?,
): MemberIdentity? {
    if (leafIndex == null) return null
    val hit = roster.firstOrNull { it.leafIndex == leafIndex } ?: return null
    return outboxB64(hit.signaturePublicKey)
}

/**
 * The group's roster as the identity resolver reads it.
 *
 * A torn / unreadable roster yields an empty list, which resolves every leaf to
 * `null` — the fail-closed direction: an unattributable message can never be
 * edited or deleted by anyone, which is strictly what an unknown author already
 * meant.
 */
internal fun rosterOf(group: Group): List<GroupMember> =
    try {
        group.members()
    } catch (cause: Throwable) {
        android.util.Log.w("AuthorIdentity", "roster read failed", cause)
        emptyList()
    }

/**
 * This device's OWN identity in [group] — its signature key, resolved THROUGH
 * the roster rather than stored, so a leaf renumber cannot make it name someone
 * else.
 *
 * This is the value every authorship decision on our own side uses: the
 * outgoing row's author stamp, our own reaction attribution, and the "read by
 * every other member" exclusion. The caller must hold the provider lock — this
 * makes two `Group` reads.
 */
internal fun ownIdentity(group: Group): MemberIdentity? {
    val leaf = try {
        group.ownLeafIndex()
    } catch (cause: Throwable) {
        android.util.Log.w("AuthorIdentity", "own leaf read failed", cause)
        return null
    }
    return resolveMemberIdentity(rosterOf(group), leaf)
}

/**
 * The author-only integrity gate for edit / delete (ADR-0023, invariant #4).
 * `true` ONLY when both identities are known and equal. Fail-closed: an unknown
 * author (a control frame, a row stored before the identity was persisted) or
 * an unresolvable op sender is never authorised.
 *
 * The comparison is on the **signature key**, and the reason is load-bearing
 * rather than stylistic: comparing MLS leaf indices here is what let a member
 * added into a departed member's vacated leaf edit and delete that member's
 * history. See the file doc.
 *
 * Reactions DO NOT go through this gate — any member may react.
 */
internal fun editDeleteAuthorized(
    author: MemberIdentity?,
    opSender: MemberIdentity?,
): Boolean {
    if (author == null || opSender == null) return false
    return author == opSender
}

/**
 * ADR-0023 — toggle `(reactor, emoji)` on a message's reaction list, returning
 * a NEW list. An existing pair is removed (toggle off), else appended.
 *
 * Keyed on the reactor's identity, never a leaf: a leaf-keyed pair let a
 * newcomer at a departed member's vacated leaf toggle that member's reaction
 * OFF — and because removing the last reactor of an emoji drops the pair
 * entirely, the chip does not merely decrement, it DISAPPEARS. It also merged
 * the newcomer's own reaction into the departed member's count in the other
 * direction.
 */
internal fun toggleReaction(
    existing: List<Reaction>,
    reactor: MemberIdentity,
    emoji: String,
): List<Reaction> {
    val at = existing.indexOfFirst { it.reactor == reactor && it.emoji == emoji }
    val next = existing.toMutableList()
    if (at >= 0) next.removeAt(at) else next.add(Reaction(reactor = reactor, emoji = emoji))
    return next.toList()
}

/**
 * ADR-0024 — fold one incoming typing signal into the ephemeral registry,
 * returning a NEW map.
 *
 *   * `isTyping == false` ⇒ clear the member immediately (an explicit stop).
 *   * `isTyping == true` whose DS row [ageMs] already exceeds
 *     [TYPING_EXPIRY_MS] ⇒ DROP (the re-sync replay guard — a
 *     full journal re-download must not resurrect a stale indicator). A `null`
 *     [ageMs] means the timestamp would not parse: treated as fresh, never
 *     dropping a live signal over a parse quirk.
 *   * else ⇒ stamp [nowMs] so the entry auto-expires one window later.
 *
 * Keyed on the sender's identity for the same reason as everything else here:
 * a leaf-keyed entry lets a newcomer inherit — and clear — the departed
 * member's typing state. Self-healing in ~6 s rather than permanent, but real
 * while it lasts, and one rule is easier to keep right than two.
 */
internal fun foldTyping(
    prev: Map<MemberIdentity, Long>,
    sender: MemberIdentity,
    isTyping: Boolean,
    ageMs: Long?,
    nowMs: Long,
): Map<MemberIdentity, Long> {
    if (!isTyping) {
        if (!prev.containsKey(sender)) return prev
        return prev.filterKeys { it != sender }
    }
    if (ageMs != null && ageMs > TYPING_EXPIRY_MS) return prev
    return prev + (sender to nowMs)
}

/**
 * ADR-0024 — the members still shown as typing at [nowMs]: every entry whose
 * stamp is within [TYPING_EXPIRY_MS]. Returns a NEW map, so the
 * caller can both read the keys and store the pruned result.
 */
internal fun pruneTyping(
    prev: Map<MemberIdentity, Long>,
    nowMs: Long,
): Map<MemberIdentity, Long> =
    prev.filterValues { nowMs - it <= TYPING_EXPIRY_MS }

/**
 * ADR-0024 — advance the monotone read watermark for [sender], returning a NEW
 * map (or the input unchanged when the value did not advance). A lower-or-equal
 * [upTo] never lowers a higher one, so re-sync replay of an older read-receipt
 * row is idempotent.
 *
 * [sender] is a signature key: the watermark is monotone and is never reset on
 * a membership change, so keying it on a leaf handed a departed member's entire
 * read history to the next member added — and the author-side "Read" tick then
 * reported messages as read by somebody who never saw them.
 */
internal fun foldReadWatermark(
    prev: Map<MemberIdentity, Long>,
    sender: MemberIdentity,
    upTo: Long,
): Map<MemberIdentity, Long> {
    val cur = prev[sender]
    if (cur != null && cur >= upTo) return prev
    return prev + (sender to upTo)
}
