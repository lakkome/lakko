// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.ui.chat.OwnReadState
import me.lakko.app.ui.chat.ownReadStateFor
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.GroupMember
import uniffi.lakko_ffi_uniffi.MessageBodyKind
import uniffi.lakko_ffi_uniffi.ParsedBody
import uniffi.lakko_ffi_uniffi.ProcessedMessage

/**
 * The leaf-reuse impersonation, and the two sibling misattributions of the same
 * shape.
 *
 * ## The defect this pins
 *
 * ADR-0023's author-only integrity gate — only a message's original author may
 * edit or delete it — compared a raw MLS leaf index. OpenMLS hands a vacated
 * leaf to the next member added, and under ADR-0045 a remove and an add compose
 * into ONE commit, so the whole sequence lands in a single poll batch. Remove
 * Bob, add Dave; Dave now holds Bob's leaf and could edit and delete Bob's
 * historical messages. On Android the author leaf was also PERSISTED, so the
 * mis-binding survived restarts for the whole retained transcript.
 *
 * Two siblings had the same shape. Reactions were attributed to the sender
 * leaf, so a newcomer at a vacated leaf could toggle a departed member's
 * reaction OFF — and since removing the last reactor drops the pair, the chip
 * did not decrement, it DISAPPEARED. Read-receipt watermarks were a leaf-keyed
 * map that is monotone and never reset on a membership change, so a newcomer
 * inherited the departed member's watermark outright and the author's "Read"
 * tick then reported messages as read by somebody who never saw them. Typing is
 * the same shape with a ~6 s lifetime.
 *
 * ## What is under test, and why it is the pipeline rather than the rules
 *
 * Every test below drives the client's real receive pipeline in miniature:
 * resolve the sender's leaf to a signature key AT RECEIPT
 * ([resolveMemberIdentity] against the roster of the epoch the message
 * decrypted at), carry the key forward, and let the production rule decide on
 * the key. What makes the pipeline the thing under test rather than the
 * individual rules is the membership change in the middle: [ROSTER_BEFORE] and
 * [ROSTER_AFTER] name DIFFERENT members at the same leaf.
 *
 * That distinction is not decorative, and
 * `a leaf resolved LATE authorises the impersonation` is kept as a permanent
 * contrast proving it: the key comparison ALONE does not close this. Resolve
 * the author's leaf late — against whatever roster is current when the op
 * arrives — and both sides resolve to Dave and the gate says yes. Delete either
 * half, the key comparison or the resolution point, and the impersonation is
 * back.
 *
 * ## Doubles
 *
 * This is a plain JVM `test` source set: `liblakko_ffi_uniffi.so` does not
 * exist here, so [toEvent]'s two FFI touchpoints are stand-ins injected through
 * its defaulted parameters, the shape the sibling `ChatServiceEnvelopeTest`
 * established. Everything that decides anything is the real production
 * function: [toEvent] itself, [resolveMemberIdentity], [editDeleteAuthorized],
 * [toggleReaction], [foldTyping], [pruneTyping], [foldReadWatermark],
 * [ownReadStateFor] and [MessageLogStore.decodeLogPayload]. `GroupMember` and
 * `ProcessedMessage` are UniFFI record types — plain data classes, safe to
 * construct on the host.
 *
 * ## Discrimination (measured, not asserted)
 *
 * Before the fix, a probe driving [toEvent] across the roster change and
 * asserting that the gate's input tells Bob from Dave failed with:
 *
 * ```text
 *   java.lang.AssertionError: the author gate cannot tell Bob from Dave:
 *   both are leaf 2. Actual: 2
 * ```
 *
 * After the fix, each rule was re-broken one at a time and the suite re-run;
 * every rule below has at least one case that fails when its rule is reverted.
 */
class AuthorIdentityTest {

    // -----------------------------------------------------------------
    // The roster, before and after the remove+add commit
    // -----------------------------------------------------------------

    /** Distinct 32-byte signature keys, byte-filled so each is obvious in a
     *  failure message. */
    private fun key(fill: Int): ByteArray = ByteArray(32) { fill.toByte() }

    private val aliceKey = key(0xA1)
    private val bobKey = key(0xB0)
    private val daveKey = key(0xDA)

    /** Epoch N: Alice at leaf 0, Bob at leaf 2. */
    private val rosterBefore = listOf(
        GroupMember(leafIndex = 0u, signaturePublicKey = aliceKey, accountKey = aliceKey),
        GroupMember(leafIndex = REUSED_LEAF, signaturePublicKey = bobKey, accountKey = bobKey),
    )

    /** Epoch N+1, after ONE commit removing Bob and adding Dave. OpenMLS hands
     *  the vacated leaf to the next Add, so Dave is at leaf 2 — the index Bob
     *  held. This is the whole hazard. */
    private val rosterAfter = listOf(
        GroupMember(leafIndex = 0u, signaturePublicKey = aliceKey, accountKey = aliceKey),
        GroupMember(leafIndex = REUSED_LEAF, signaturePublicKey = daveKey, accountKey = daveKey),
    )

    /** ADR-0050 — Alice's two devices are two leaves bound to one account. */
    @Test
    fun `two devices of one account resolve to one author`() {
        val phone = key(0x01)
        val laptop = key(0x02)
        val roster = listOf(
            GroupMember(leafIndex = 0u, signaturePublicKey = phone, accountKey = aliceKey),
            GroupMember(leafIndex = 1u, signaturePublicKey = laptop, accountKey = aliceKey),
            GroupMember(leafIndex = 2u, signaturePublicKey = bobKey, accountKey = ByteArray(0)),
        )
        assertEquals(resolveMemberIdentity(roster, 0u), resolveMemberIdentity(roster, 1u))
        assertEquals(outboxB64(aliceKey), resolveMemberIdentity(roster, 1u))
        // An unresolved credential falls back to the leaf key.
        assertEquals(outboxB64(bobKey), resolveMemberIdentity(roster, 2u))
        assertEquals(listOf(outboxB64(aliceKey), outboxB64(bobKey)), personKeysOf(roster))
    }

    private val alice = resolveMemberIdentity(rosterBefore, 0u)!!
    private val bob = resolveMemberIdentity(rosterBefore, REUSED_LEAF)!!
    private val dave = resolveMemberIdentity(rosterAfter, REUSED_LEAF)!!

    private companion object {
        /** The leaf Bob held and Dave inherited. */
        const val REUSED_LEAF: UInt = 2u
    }

    // -----------------------------------------------------------------
    // The receive pipeline, driven through the real classifier
    // -----------------------------------------------------------------

    private fun body(
        kind: MessageBodyKind,
        text: String = "",
        target: Long? = null,
        emoji: String? = null,
        isTyping: Boolean? = null,
    ): ParsedBody = ParsedBody(
        kind = kind,
        text = text,
        attachments = emptyList(),
        target = target,
        emoji = emoji,
        isTyping = isTyping,
        // ADR-0052 - a reply is a `CHAT` that also points somewhere; nothing here is one.
        replyTo = null,
        replyAuthor = null,
        replyExcerpt = null,
    )

    /**
     * One DS row through the REAL [toEvent], resolved against [roster] — which
     * stands in for "the roster at the epoch this row decrypted at". Swapping
     * the roster between calls is what models the remove+add commit landing
     * between two rows of one poll batch.
     */
    private fun receive(
        roster: List<GroupMember>,
        parsed: ParsedBody,
        senderLeaf: UInt? = REUSED_LEAF,
        messageId: Long = 1L,
        createdAt: String = "2026-08-24T00:00:00Z",
    ): IncomingChatEvent = toEvent(
        messageId = messageId,
        createdAt = createdAt,
        processed = ProcessedMessage.Application(ByteArray(0), senderLeaf, null),
        resolveSender = { leaf -> resolveMemberIdentity(roster, leaf) },
        resolveLeafSignatureKey = { leaf -> resolveLeafSignatureKey(roster, leaf) },
        isCommitterBeacon = { false },
        parseBody = { parsed },
    )

    // -----------------------------------------------------------------
    // resolveMemberIdentity — the resolution rule itself
    // -----------------------------------------------------------------

    @Test
    fun `resolves a leaf to the member's signature key`() {
        assertEquals(alice, resolveMemberIdentity(rosterBefore, 0u))
    }

    @Test
    fun `resolves ONE leaf to DIFFERENT members across the commit`() {
        // The premise of every test below, stated once: the same leaf index
        // names two different people either side of the commit.
        assertNotEquals(bob, dave)
        assertEquals(bob, resolveMemberIdentity(rosterBefore, REUSED_LEAF))
        assertEquals(dave, resolveMemberIdentity(rosterAfter, REUSED_LEAF))
    }

    @Test
    fun `is null for a leaf no member holds, and for a null leaf`() {
        assertNull(resolveMemberIdentity(rosterBefore, 7u))
        assertNull(resolveMemberIdentity(rosterBefore, null))
    }

    @Test
    fun `is null against an unreadable roster (fail closed)`() {
        // What `rosterOf` yields when `group.members()` throws.
        assertNull(resolveMemberIdentity(emptyList(), REUSED_LEAF))
    }

    @Test
    fun `treats leaf 0 as a real leaf, not a falsy miss`() {
        assertEquals(alice, resolveMemberIdentity(rosterBefore, 0u))
    }

    // -----------------------------------------------------------------
    // The headline: edit / delete impersonation
    // -----------------------------------------------------------------

    /** Bob authors a message at epoch N. The receive loop resolves his leaf
     *  THERE, against the roster that names him. */
    private fun bobsMessageAuthor(): MemberIdentity? =
        (receive(rosterBefore, body(MessageBodyKind.CHAT, text = "Bob's message")) as
            IncomingChatEvent.Application).senderKey

    /** One commit later Dave holds leaf 2 and sends an op. His op's senderLeaf
     *  is 2 — indistinguishable from Bob's, which is precisely why the leaf
     *  cannot be the thing compared. */
    private fun davesOp(kind: MessageBodyKind): IncomingChatEvent.Op =
        receive(
            rosterAfter,
            body(kind, text = "rewritten by Dave", target = 1L),
            messageId = 9L,
        ) as IncomingChatEvent.Op

    @Test
    fun `the receive pipeline distinguishes a departed member from the newcomer at their leaf`() {
        val author = bobsMessageAuthor()
        val op = davesOp(MessageBodyKind.EDIT)

        // The leaf really is the same on both — the pre-fix state of the world.
        assertEquals(REUSED_LEAF, op.senderLeaf)
        assertNotEquals(
            "the author gate cannot tell Bob from Dave: both are leaf ${op.senderLeaf}",
            author,
            op.senderKey,
        )
    }

    @Test
    fun `REFUSES a newcomer at a vacated leaf EDITING the departed member's message`() {
        val op = davesOp(MessageBodyKind.EDIT)
        assertEquals(dave, op.senderKey)
        assertFalse(editDeleteAuthorized(bobsMessageAuthor(), op.senderKey))
    }

    @Test
    fun `REFUSES a newcomer at a vacated leaf DELETING the departed member's message`() {
        // Delete runs the identical gate — pinned separately so a change that
        // only re-secures edit cannot pass.
        val op = davesOp(MessageBodyKind.DELETE)
        assertEquals(dave, op.senderKey)
        assertFalse(editDeleteAuthorized(bobsMessageAuthor(), op.senderKey))
    }

    @Test
    fun `still lets the real author edit their own message`() {
        // The refusal must not be "refuse everything": Bob editing at epoch N
        // resolves to the same identity he authored under.
        val op = receive(
            rosterBefore,
            body(MessageBodyKind.EDIT, text = "fixed a typo", target = 1L),
            messageId = 9L,
        ) as IncomingChatEvent.Op
        assertTrue(editDeleteAuthorized(bobsMessageAuthor(), op.senderKey))
    }

    @Test
    fun `still refuses a plain non-author who never held the leaf`() {
        val op = receive(
            rosterBefore,
            body(MessageBodyKind.EDIT, text = "not mine to edit", target = 1L),
            senderLeaf = 0u,
            messageId = 9L,
        ) as IncomingChatEvent.Op
        assertEquals(alice, op.senderKey)
        assertFalse(editDeleteAuthorized(bobsMessageAuthor(), op.senderKey))
    }

    @Test
    fun `fails closed on an unresolvable author (a row stored before the key was persisted)`() {
        assertFalse(editDeleteAuthorized(null, dave))
    }

    @Test
    fun `fails closed on an unresolvable op sender (torn roster)`() {
        val op = receive(
            emptyList(),
            body(MessageBodyKind.EDIT, text = "who am I", target = 1L),
            messageId = 9L,
        ) as IncomingChatEvent.Op
        assertNull(op.senderKey)
        assertFalse(editDeleteAuthorized(bob, op.senderKey))
    }

    @Test
    fun `a leaf resolved LATE authorises the impersonation`() {
        // THE contrast, kept permanently. The wrong shape: store the author's
        // LEAF and resolve it when the op arrives, against whatever roster is
        // current then. Both sides resolve to Dave and the gate says yes.
        //
        // The rule that stops the impersonation is therefore NOT the key
        // comparison on its own — it is resolving the author's leaf at RECEIPT
        // and carrying the key. Delete either half and it is back.
        val resolvedLate = resolveMemberIdentity(rosterAfter, REUSED_LEAF)
        val opSender = davesOp(MessageBodyKind.EDIT).senderKey
        assertTrue(
            "a late-resolved author is the impersonation — this MUST be true",
            editDeleteAuthorized(resolvedLate, opSender),
        )
        // ...whereas the identity resolved at receipt refuses the same op.
        assertFalse(editDeleteAuthorized(bobsMessageAuthor(), opSender))
    }

    // -----------------------------------------------------------------
    // Sibling 1 — reactions
    // -----------------------------------------------------------------

    private fun react(prev: List<Reaction>, who: MemberIdentity): List<Reaction> =
        toggleReaction(prev, who, "👍")

    @Test
    fun `a newcomer at a vacated leaf cannot DESTROY the departed member's reaction`() {
        // Bob reacts at epoch N; Dave, at Bob's leaf, reacts at epoch N+1.
        val afterBob = react(emptyList(), bob)
        val afterDave = react(afterBob, dave)

        assertEquals(
            "Dave's reaction destroyed Bob's chip instead of adding to it",
            setOf(bob, dave),
            afterDave.map { it.reactor }.toSet(),
        )
        // The damage under a leaf key was worse than "toggle off": removing the
        // last reactor prunes the pair, so the chip vanished entirely.
        assertTrue(afterDave.any { it.emoji == "👍" })
    }

    @Test
    fun `a newcomer's reaction does not MERGE into the departed member's count`() {
        val m = react(react(emptyList(), bob), dave)
        assertEquals(2, m.count { it.emoji == "👍" })
    }

    @Test
    fun `the real reactor can still toggle their OWN reaction off`() {
        val m = react(react(emptyList(), bob), bob)
        assertTrue("Bob's own second tap must remove his reaction", m.isEmpty())
    }

    @Test
    fun `the reaction fold attributes to the identity the pipeline resolved`() {
        // Drive it end to end: the reaction op Dave sends carries Dave's key,
        // not Bob's, even though its leaf is Bob's.
        val op = receive(
            rosterAfter,
            body(MessageBodyKind.REACTION, target = 1L, emoji = "👍"),
            messageId = 9L,
        ) as IncomingChatEvent.Op
        val folded = toggleReaction(react(emptyList(), bob), op.senderKey!!, op.emoji!!)
        assertEquals(listOf(bob, dave), folded.map { it.reactor })
    }

    @Test
    fun `the own-reaction sentinel can never collide with a real identity`() {
        // A real identity is base64; `?` is outside that alphabet. This is the
        // property the old numeric sentinel had against a real leaf index.
        for (id in listOf(alice, bob, dave)) {
            assertNotEquals(OWN_REACTION_SENTINEL, id)
            assertFalse("a real identity must not contain '?'", id.contains('?'))
        }
        assertTrue(OWN_REACTION_SENTINEL.contains('?'))
    }

    // -----------------------------------------------------------------
    // Sibling 2 — read-receipt watermarks
    // -----------------------------------------------------------------

    @Test
    fun `a newcomer at a vacated leaf does NOT inherit the departed member's watermark`() {
        // Bob read up to message 50, then was removed.
        val m = foldReadWatermark(emptyMap(), bob, 50L)
        // Dave arrives at Bob's leaf having read nothing.
        assertNull("Dave inherited Bob's read history", m[dave])
        assertEquals(50L, m[bob])
    }

    @Test
    fun `does not report a message as read by someone who never saw it`() {
        // Alice's own view: Bob (departed) read to 50; Dave (newcomer) has read
        // only to 10. Message 50 must NOT show as Read.
        var m = foldReadWatermark(emptyMap(), bob, 50L)
        m = foldReadWatermark(m, dave, 10L)
        assertEquals(
            OwnReadState.Sent,
            ownReadStateFor(50L, own = alice, others = setOf(dave), readUpToByMember = m),
        )
        // It flips only once Dave genuinely catches up.
        m = foldReadWatermark(m, dave, 50L)
        assertEquals(
            OwnReadState.Read,
            ownReadStateFor(50L, own = alice, others = setOf(dave), readUpToByMember = m),
        )
    }

    @Test
    fun `stays monotone per member across the membership change`() {
        var m = foldReadWatermark(emptyMap(), bob, 50L)
        // Dave's first receipt is LOWER than Bob's — under a leaf key it would
        // be swallowed as a non-advance and Dave would keep Bob's 50. Under an
        // identity key it is Dave's own first watermark.
        m = foldReadWatermark(m, dave, 10L)
        assertEquals(50L, m[bob])
        assertEquals(10L, m[dave])
    }

    @Test
    fun `a replayed older receipt never lowers a member's watermark`() {
        var m = foldReadWatermark(emptyMap(), bob, 50L)
        m = foldReadWatermark(m, bob, 10L)
        assertEquals(50L, m[bob])
    }

    @Test
    fun `a leaf-keyed watermark WOULD hand the newcomer the departed member's history`() {
        // The permanent contrast for this sibling, mirroring
        // `a leaf resolved LATE authorises the impersonation`. Fold Bob's
        // watermark under the LEAF the two of them share, then ask the read
        // tick about the newcomer reading under that same leaf: it answers Read
        // for a message Dave never saw.
        val sharedLeafKey = "leaf-$REUSED_LEAF"
        val leafKeyed = foldReadWatermark(emptyMap(), sharedLeafKey, 50L)
        assertEquals(
            "under a leaf key the newcomer inherits — this MUST be Read",
            OwnReadState.Read,
            ownReadStateFor(
                50L,
                own = alice,
                others = setOf(sharedLeafKey),
                readUpToByMember = leafKeyed,
            ),
        )
        // Under identities the same two folds keep them apart.
        val idKeyed = foldReadWatermark(emptyMap(), bob, 50L)
        assertEquals(
            OwnReadState.Sent,
            ownReadStateFor(50L, own = alice, others = setOf(dave), readUpToByMember = idKeyed),
        )
    }

    @Test
    fun `the watermark fold keys on the identity the pipeline resolved`() {
        val receipt = receive(
            rosterAfter,
            body(MessageBodyKind.READ_RECEIPT, target = 10L),
            messageId = 9L,
        ) as IncomingChatEvent.ReadReceipt
        val m = foldReadWatermark(foldReadWatermark(emptyMap(), bob, 50L), receipt.senderKey!!, 10L)
        assertEquals(50L, m[bob])
        assertEquals(10L, m[dave])
    }

    // -----------------------------------------------------------------
    // Sibling 3 — typing
    // -----------------------------------------------------------------

    @Test
    fun `a newcomer at a vacated leaf does not inherit the departed member's typing state`() {
        // Same shape as the watermark, but self-healing in ~6 s, so the window
        // is narrow rather than permanent. Keyed on the identity anyway: the
        // misattribution is real while it lasts, and one rule is easier to keep
        // right than two.
        val t0 = 1_000_000L
        val m = foldTyping(emptyMap(), bob, isTyping = true, ageMs = 0L, nowMs = t0)
        assertEquals(setOf(bob), pruneTyping(m, t0 + 1).keys)
        assertFalse(m.containsKey(dave))
    }

    @Test
    fun `a newcomer's stop does not clear the departed member's entry`() {
        val t0 = 1_000_000L
        var m = foldTyping(emptyMap(), bob, isTyping = true, ageMs = 0L, nowMs = t0)
        m = foldTyping(m, dave, isTyping = false, ageMs = 0L, nowMs = t0)
        assertTrue("Dave's stop cleared Bob's typing state", m.containsKey(bob))
    }

    @Test
    fun `a stale re-synced typing signal is dropped`() {
        // The ADR-0024 replay guard, kept green through the rekey: a full
        // journal re-download must not resurrect an old indicator.
        val t0 = 1_000_000L
        val m = foldTyping(
            emptyMap(),
            bob,
            isTyping = true,
            ageMs = TYPING_EXPIRY_MS + 1,
            nowMs = t0,
        )
        assertTrue(m.isEmpty())
    }

    @Test
    fun `an entry ages out of the window`() {
        val t0 = 1_000_000L
        val m = foldTyping(emptyMap(), bob, isTyping = true, ageMs = null, nowMs = t0)
        assertEquals(setOf(bob), pruneTyping(m, t0 + TYPING_EXPIRY_MS).keys)
        assertTrue(pruneTyping(m, t0 + TYPING_EXPIRY_MS + 1).isEmpty())
    }

    @Test
    fun `a leaf-keyed typing registry WOULD let the newcomer clear the departed member`() {
        // The same permanent contrast for typing: under the shared leaf key,
        // Dave's explicit stop erases Bob's entry.
        val t0 = 1_000_000L
        val sharedLeafKey = "leaf-$REUSED_LEAF"
        var leafKeyed = foldTyping(emptyMap(), sharedLeafKey, true, ageMs = 0L, nowMs = t0)
        leafKeyed = foldTyping(leafKeyed, sharedLeafKey, false, ageMs = 0L, nowMs = t0)
        assertTrue(
            "under a leaf key the newcomer's stop erases the departed member — " +
                "this MUST be empty",
            leafKeyed.isEmpty(),
        )
        // Under identities the same two folds keep them apart.
        var idKeyed = foldTyping(emptyMap(), bob, true, ageMs = 0L, nowMs = t0)
        idKeyed = foldTyping(idKeyed, dave, false, ageMs = 0L, nowMs = t0)
        assertEquals(setOf(bob), idKeyed.keys)
    }

    @Test
    fun `the typing fold keys on the identity the pipeline resolved`() {
        val signal = receive(
            rosterAfter,
            body(MessageBodyKind.TYPING, isTyping = true),
            messageId = 9L,
        ) as IncomingChatEvent.Typing
        assertEquals(dave, signal.senderKey)
        val t0 = 1_000_000L
        var m = foldTyping(emptyMap(), bob, isTyping = true, ageMs = 0L, nowMs = t0)
        m = foldTyping(m, signal.senderKey!!, isTyping = true, ageMs = 0L, nowMs = t0)
        assertEquals(setOf(bob, dave), m.keys)
    }

    // -----------------------------------------------------------------
    // Persistence — the author identity round-trips; a pre-existing row does not
    // -----------------------------------------------------------------

    @Test
    fun `the author identity round-trips, so edit-delete survives a reload`() {
        val restored = PersistedMessage(
            messageId = 1L,
            direction = "incoming",
            text = "Bob's message",
            attachments = emptyList(),
            createdAt = "2026-08-24T00:00:00Z",
            authorKeyB64 = bob,
        ).toChatMessage()!!

        assertEquals(bob, restored.authorKey)
        // Which is the point of persisting it at all: before this, every
        // reloaded row on web had an unknown author and the gate refused the
        // author's own edit. Android persisted a LEAF instead, which is worse —
        // it round-tripped cleanly and named whoever inherited it.
        assertTrue(editDeleteAuthorized(restored.authorKey, bob))
        assertFalse(editDeleteAuthorized(restored.authorKey, dave))
    }

    @Test
    fun `a PRE-EXISTING stored row still decodes, and fails the gate CLOSED`() {
        // A real envelope as the PRE-FIX build wrote it: `authorLeaf` and
        // `reactorLeaf` present, `authorKeyB64` / `reactorKeyB64` absent. It
        // must still decode — discarding it would wipe the transcript to fix
        // nothing — and the row must come back with NO author.
        val preFixJson = """
            {"version":1,"groups":{"aabb":{"cursor":7,"messages":[
              {"messageId":1,"direction":"incoming","text":"Bob's message",
               "attachments":[],"createdAt":"2026-08-24T00:00:00Z",
               "authorLeaf":2,"edited":false,"deleted":false,
               "reactions":[{"reactorLeaf":2,"emoji":"x"}]}
            ]}}}
        """.trimIndent()

        val payload = MessageLogStore.decodeLogPayload(preFixJson)
        assertTrue(
            "a pre-existing envelope must not be discarded — that would wipe the transcript",
            payload != null,
        )
        val stored = payload!!.groups.getValue("aabb").messages.single()
        assertEquals(7L, payload.groups.getValue("aabb").cursor)
        assertEquals("Bob's message", stored.text)
        assertNull("a retired leaf must not be revived as an identity", stored.authorKeyB64)

        val restored = stored.toChatMessage()!!
        assertNull(restored.authorKey)
        // Fail-closed: unknown author authorises nobody — not the newcomer at
        // the leaf the row used to name, and not the reader either.
        assertFalse(editDeleteAuthorized(restored.authorKey, dave))
        assertFalse(editDeleteAuthorized(restored.authorKey, bob))
        // ...and an unattributable reaction is DROPPED rather than restored
        // under a guessed reactor (nobody could ever toggle it off).
        assertTrue(restored.reactions.isEmpty())
    }

    @Test
    fun `a pre-existing row is never MISATTRIBUTED to the reader`() {
        // The tempting shortcut for an OUTGOING row — "it is ours, so assume
        // our current key" — would invent an author the store never recorded.
        // Unknown must stay unknown.
        val restored = PersistedMessage(
            messageId = 1L,
            direction = "outgoing",
            text = "mine, once",
            attachments = emptyList(),
            createdAt = "2026-08-24T00:00:00Z",
        ).toChatMessage()!!
        assertNull(restored.authorKey)
        assertNotEquals(alice, restored.authorKey)
    }

    @Test
    fun `the stored shape carries no leaf at all`() {
        // A leaf would round-trip cleanly and then name whoever inherited it —
        // which is exactly how this survived restarts on Android. The written
        // value is a key or null, and nothing else.
        val written = PersistedMessage.from(
            ChatMessage(
                messageId = 1L,
                direction = Direction.Incoming,
                text = "hi",
                createdAt = "2026-08-24T00:00:00Z",
                authorKey = bob,
                reactions = listOf(Reaction(reactor = dave, emoji = "x")),
            ),
        )
        assertEquals(bob, written.authorKeyB64)
        assertEquals(listOf(dave), written.reactions.map { it.reactorKeyB64 })
        assertFalse(
            "the persisted shape must not carry a leaf field",
            PersistedMessage::class.java.declaredFields.any {
                it.name.contains("Leaf", ignoreCase = true)
            } ||
                PersistedReaction::class.java.declaredFields.any {
                    it.name.contains("Leaf", ignoreCase = true)
                },
        )
    }
}
