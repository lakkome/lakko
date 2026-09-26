// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.push.GroupPushRouting
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.MessageBodyKind
import uniffi.lakko_ffi_uniffi.ParsedBody
import uniffi.lakko_ffi_uniffi.ProcessedMessage

/**
 * ADR-0045 — the FOREGROUND receive loop classifies a committer-liveness
 * beacon as a service envelope, never as a chat bubble.
 *
 * ## The defect this pins
 *
 * The beacon is a 27-byte, field-less envelope (`domain(26) || version(1)`)
 * every owner and admin emits on a fixed interval. [toEvent] filtered exactly
 * two service-envelope kinds — call signaling and the ADR-0042 / ADR-0044
 * push-routing secret — and then fell through to `parseBodyEnvelope`, whose
 * lossy-UTF-8 fallback is CORRECT on its own terms (pre-envelope legacy
 * plaintext must still render) and therefore turned the beacon into a visible
 * bubble reading `lakko/committer-beacon/v1` plus two control bytes.
 *
 * ## The doubles, and why they are faithful
 *
 * This is a plain JVM `test` source set: `liblakko_ffi_uniffi.so` does not
 * exist here, so both FFI touchpoints in [toEvent] are stand-ins, injected
 * through its defaulted parameters. Two things make that safe, and both are
 * asserted rather than assumed:
 *
 *  1. **`parseBody` DEGRADES, it does not throw.** This is THE trap in this
 *     area: a stand-in that threw on binary input would make every case below
 *     pass with the beacon arm deleted, because the beacon would take a
 *     (nonexistent) throw path instead of becoming a bubble. [lossyParseBody]
 *     models the real `parse_body` contract — unrecognised bytes come back as
 *     [MessageBodyKind.CHAT] carrying the lossy-UTF-8 text — and
 *     `the parse double degrades binary input to chat, it never throws` pins
 *     exactly that, so the double cannot silently drift into the shape that
 *     would make this suite vacuous.
 *  2. **The beacon double is not trivially true.** [fakeBeaconArm] applies the
 *     same three gates as the Rust codec (domain prefix, exact length, schema
 *     version) and `the beacon double rejects everything that is not a beacon`
 *     pins that it rejects a chat envelope, a routing-secret envelope, a
 *     call-signaling payload, and a version-bumped beacon — so the arm cannot
 *     be passing merely because the double says yes to everything.
 *
 * The production wiring these stand in for is one line each
 * (`ChatClient.kt`'s `committerBeaconArm` / `::parseBodyEnvelope` defaults),
 * and the pure half of the beacon arm — [looksLikeCommitterBeacon] — is the
 * REAL production function, driven directly below with no double at all.
 *
 * ## Fixture hygiene
 *
 * The beacon's domain separator ends in a NUL byte. It is appended
 * PROGRAMMATICALLY ([beaconDomain]) and never written as a literal, because a
 * raw NUL makes git treat the source file as binary — the same rule the
 * sibling `PushPreviewDecryptTest` follows. [beaconEnvelopeLen] is likewise
 * DERIVED from the fixture builder rather than written as `27`.
 *
 * ## Discrimination (verified by running the broken variant)
 *
 * With the `isCommitterBeacon` arm removed from [toEvent], five of the thirteen
 * cases fail, each reporting the bubble text itself:
 *   - `a committer beacon is classified as a beacon, never a chat event`:
 *     `a committer beacon must classify as CommitterBeacon, got
 *     Application(messageId=1, text=lakko/committer-beacon/v1??, ...)`;
 *   - `a beacon never reaches the timeline as text`:
 *     `the beacon rendered as a chat bubble reading
 *     'lakko/committer-beacon/v1??'`;
 *   - `a beacon carries the sender leaf and the DS timestamp` and
 *     `a beacon from an unattributable sender still never becomes a bubble`:
 *     `expected CommitterBeacon, got Application(...)`;
 *   - `each envelope kind classifies independently`:
 *     `row 1: Application(... text=lakko/committer-beacon/v1??, ...)`.
 * The other eight pass either way and say so where they sit: they are the
 * regression guards for the two arms that already existed, the two
 * double-faithfulness pins above, and the pure-peek unit cases.
 */
class ChatServiceEnvelopeTest {

    // -----------------------------------------------------------------
    // Fixtures — every NUL appended programmatically
    // -----------------------------------------------------------------

    /** `lakko/committer-beacon/v1` + a trailing NUL — 26 bytes. */
    private val beaconDomain: ByteArray =
        "lakko/committer-beacon/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0)

    /** A well-formed ADR-0045 beacon: `domain(26) || version(1)=0x01`.
     *  FIELD-LESS, so every beacon on the wire is byte-identical. */
    private fun committerBeacon(version: Int = 0x01): ByteArray =
        beaconDomain + byteArrayOf(version.toByte())

    /** Stand-in for the FFI's `committerBeaconEnvelopeLen()` — DERIVED from
     *  the fixture builder, never a literal `27`, so the length under test and
     *  the bytes it is tested against cannot diverge. */
    private val beaconEnvelopeLen: Int = committerBeacon().size

    /** `lakko/push-routing-secret/v1` + a trailing NUL — 29 bytes. */
    private val routingDomain: ByteArray =
        "lakko/push-routing-secret/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0)

    /** A well-formed ADR-0044 v2 routing-secret envelope:
     *  `domain(29) || version(1)=0x02 || mint_epoch(8) || secret(32)`. */
    private fun routingSecretEnvelope(): ByteArray {
        val out = ByteArray(GroupPushRouting.ENVELOPE_LEN)
        routingDomain.copyInto(out, 0)
        out[29] = 0x02
        for (i in 38 until out.size) out[i] = 0xAB.toByte()
        return out
    }

    /** A `CallSignalingPayload` proto: first byte is one of the oneof body
     *  tags (fields 10..13). */
    private fun callSignalingPayload(): ByteArray =
        byteArrayOf(0x62, 0x04, 0x01, 0x02, 0x03, 0x04)

    /** An ADR-0023 chat envelope — JSON, so it opens with `{` (0x7B). */
    private fun chatEnvelope(text: String): ByteArray =
        """{"v":1,"text":"$text","attachments":[]}""".toByteArray(Charsets.UTF_8)

    // -----------------------------------------------------------------
    // Doubles for the two FFI touchpoints (see the class doc)
    // -----------------------------------------------------------------

    /** Stand-in for the native `decodeCommitterBeacon` composed with the
     *  production peek: the same three fail-closed gates the Rust codec
     *  applies — domain prefix, THEN exact length, THEN schema version. */
    private fun fakeBeaconArm(bytes: ByteArray): Boolean =
        bytes.size == beaconEnvelopeLen &&
            bytes.copyOfRange(0, beaconDomain.size).contentEquals(beaconDomain) &&
            bytes[beaconDomain.size].toInt() == 0x01

    /** Stand-in for the native `parseBodyEnvelope`, modelling the load-bearing
     *  half of its contract: an ADR-0023 JSON envelope parses to its text, and
     *  ANYTHING ELSE — including binary — DEGRADES to a chat body carrying the
     *  lossy-UTF-8 bytes. It never throws. That degradation is precisely why
     *  the beacon needs its own arm ahead of this. */
    private fun lossyParseBody(bytes: ByteArray): ParsedBody {
        val raw = String(bytes, Charsets.UTF_8)
        val text = Regex("\"text\":\"([^\"]*)\"").find(raw)?.groupValues?.get(1) ?: raw
        return ParsedBody(
            kind = MessageBodyKind.CHAT,
            text = text,
            attachments = emptyList(),
            target = null,
            emoji = null,
            isTyping = null,
            // ADR-0052 - a reply is a `CHAT` that also points somewhere; nothing here is one.
            replyTo = null,
            replyAuthor = null,
            replyExcerpt = null,
            forwardedFrom = null,
            forwardOriginGroupB64 = null,
            forwardOriginMessageId = null,
        )
    }

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    /** Run one plaintext through the real classification chain, as a DS row
     *  would arrive after MLS decrypt + role verification. */
    private fun classify(
        payload: ByteArray,
        senderLeaf: UInt? = 3u,
        messageId: Long = 1L,
    ): IncomingChatEvent =
        toEvent(
            messageId = messageId,
            createdAt = "2026-08-24T00:00:00Z",
            processed = ProcessedMessage.Application(payload, senderLeaf, null),
            // This suite is about ENVELOPE CLASSIFICATION, not attribution: a
            // resolver that names nobody keeps every event here unattributed,
            // which is what a control frame looks like and changes no verdict
            // below. The attribution rule has its own suite
            // (`AuthorIdentityTest`).
            resolveSender = { null },
            resolveLeafSignatureKey = { null },
            isCommitterBeacon = ::fakeBeaconArm,
            parseBody = ::lossyParseBody,
        )

    /** The user-visible text an event would put on the timeline, or `null` for
     *  an event that renders none. */
    private fun timelineText(event: IncomingChatEvent): String? =
        (event as? IncomingChatEvent.Application)?.text

    // -----------------------------------------------------------------
    // The double-faithfulness pins (see the class doc)
    // -----------------------------------------------------------------

    @Test
    fun `the parse double degrades binary input to chat, it never throws`() {
        // THE trap: a throwing stand-in would make every beacon case below
        // pass with the arm deleted. Assert the degradation explicitly, on the
        // exact bytes the arm exists to intercept.
        val parsed = lossyParseBody(committerBeacon())

        assertEquals(MessageBodyKind.CHAT, parsed.kind)
        assertTrue(
            "expected the lossy fallback text, got '${parsed.text}'",
            parsed.text.contains("lakko/committer-beacon/v1"),
        )
    }

    @Test
    fun `the beacon double rejects everything that is not a beacon`() {
        // The arm cannot be passing merely because the double says yes to
        // everything: it must say no to every other wire shape this loop sees.
        assertTrue(fakeBeaconArm(committerBeacon()))
        assertFalse(fakeBeaconArm(chatEnvelope("hello")))
        assertFalse(fakeBeaconArm(routingSecretEnvelope()))
        assertFalse(fakeBeaconArm(callSignalingPayload()))
        assertFalse(fakeBeaconArm(committerBeacon(version = 0xFF)))
        assertFalse(fakeBeaconArm(ByteArray(0)))
    }

    // -----------------------------------------------------------------
    // The pure production peek, driven with no double at all
    // -----------------------------------------------------------------

    @Test
    fun `the production peek accepts a beacon and rejects the near misses`() {
        val alwaysDecodes: (ByteArray) -> Boolean = { true }

        assertTrue(
            looksLikeCommitterBeacon(committerBeacon(), beaconEnvelopeLen, alwaysDecodes),
        )
        // Wrong length — a chat envelope, a routing envelope, empty bytes.
        assertFalse(
            looksLikeCommitterBeacon(chatEnvelope("hi"), beaconEnvelopeLen, alwaysDecodes),
        )
        assertFalse(
            looksLikeCommitterBeacon(routingSecretEnvelope(), beaconEnvelopeLen, alwaysDecodes),
        )
        assertFalse(
            looksLikeCommitterBeacon(ByteArray(0), beaconEnvelopeLen, alwaysDecodes),
        )
        // Right length, wrong leading domain byte — a 27-byte legacy plaintext.
        val lookalike = ByteArray(beaconEnvelopeLen) { 'x'.code.toByte() }
        assertFalse(
            looksLikeCommitterBeacon(lookalike, beaconEnvelopeLen, alwaysDecodes),
        )
    }

    @Test
    fun `the production peek gives the codec the last word`() {
        // The peek gates the codec; it does not replace it. A decoder that
        // refuses must sink the verdict even when the cheap gates pass — which
        // is what makes the arm fail-closed on an unknown schema version rather
        // than silently swallowing a future v2 beacon.
        val neverDecodes: (ByteArray) -> Boolean = { false }

        assertFalse(
            looksLikeCommitterBeacon(committerBeacon(), beaconEnvelopeLen, neverDecodes),
        )
    }

    @Test
    fun `the production peek reads the length it is given, not a hardcoded 27`() {
        // Discriminates "sized by the FFI constant" from "sized by a literal":
        // hand it a wrong length and a genuine beacon must stop matching.
        assertFalse(
            looksLikeCommitterBeacon(committerBeacon(), beaconEnvelopeLen + 1) { true },
        )
    }

    // -----------------------------------------------------------------
    // The beacon arm inside the classification chain
    // -----------------------------------------------------------------

    @Test
    fun `a committer beacon is classified as a beacon, never a chat event`() {
        val event = classify(committerBeacon())

        assertTrue(
            "a committer beacon must classify as CommitterBeacon, got $event",
            event is IncomingChatEvent.CommitterBeacon,
        )
    }

    @Test
    fun `a beacon never reaches the timeline as text`() {
        // Whatever the classification, no event this loop emits may carry the
        // domain separator as user-visible text. Written against the rendered
        // text rather than the variant so it keeps discriminating if the arm
        // is ever re-routed to a different variant.
        val rendered = timelineText(classify(committerBeacon())) ?: ""

        assertFalse(
            "the beacon rendered as a chat bubble reading '$rendered'",
            rendered.contains("committer-beacon"),
        )
    }

    @Test
    fun `a beacon carries the sender leaf and the DS timestamp`() {
        // ADR-0045: the beacon deliberately names neither its sender nor its
        // age, so the fold's only sources are the MLS-authenticated leaf and
        // the DS row. Surfacing both is the reason this is an event and not a
        // silent drop inside the classifier.
        val event = classify(committerBeacon(), senderLeaf = 7u, messageId = 42L)

        assertTrue("expected CommitterBeacon, got $event", event is IncomingChatEvent.CommitterBeacon)
        val beacon = event as IncomingChatEvent.CommitterBeacon
        assertEquals(42L, beacon.messageId)
        assertEquals(7u, beacon.senderLeaf)
        assertEquals("2026-08-24T00:00:00Z", beacon.createdAt)
    }

    @Test
    fun `a beacon from an unattributable sender still never becomes a bubble`() {
        // A control frame / external sender surfaces no leaf. The fold must
        // treat that as unusable (an unattributable beacon proves no candidate
        // is live), but it is still emphatically not a chat message.
        val event = classify(committerBeacon(), senderLeaf = null)

        assertTrue("expected CommitterBeacon, got $event", event is IncomingChatEvent.CommitterBeacon)
        assertNull((event as IncomingChatEvent.CommitterBeacon).senderLeaf)
    }

    @Test
    fun `an unknown beacon version is not swallowed by the arm`() {
        // Fail-closed on the version byte: the arm claims only what the codec
        // accepts. A future v2 beacon falls through to the chat parser like any
        // other unrecognised plaintext rather than being eaten by a
        // length-only match.
        val event = classify(committerBeacon(version = 0xFF))

        assertTrue("expected Application, got $event", event is IncomingChatEvent.Application)
    }

    // -----------------------------------------------------------------
    // The pre-existing arms still work
    // -----------------------------------------------------------------

    @Test
    fun `a real chat message is still a chat event`() {
        val event = classify(chatEnvelope("hello"))

        assertTrue("expected Application, got $event", event is IncomingChatEvent.Application)
        assertEquals("hello", (event as IncomingChatEvent.Application).text)
    }

    @Test
    fun `call signaling is still lifted out of the timeline`() {
        val event = classify(callSignalingPayload())

        assertTrue("expected CallSignaling, got $event", event is IncomingChatEvent.CallSignaling)
    }

    @Test
    fun `group call control bodies are lifted out of the timeline`() {
        // Fields 14..16. Before the sniff knew these tags they fell through to
        // the chat parser, whose lossy-UTF-8 fallback rendered them as a
        // garbage bubble.
        for (payload in listOf(
            me.lakko.app.calls.GroupCallWireFixtures.groupStart(),
            me.lakko.app.calls.GroupCallWireFixtures.groupJoin(),
            me.lakko.app.calls.GroupCallWireFixtures.groupLeave(),
        )) {
            val event = classify(payload)
            assertTrue(
                "expected CallSignaling for tag 0x${(payload[0].toInt() and 0xFF).toString(16)}, got $event",
                event is IncomingChatEvent.CallSignaling,
            )
        }
    }

    @Test
    fun `a legacy plain text opening with r or z is still a chat message`() {
        // 0x72 and 0x7A are `r` and `z`; the group tags must not swallow text.
        for (text in listOf("rabbit", "zebra", "r", "z")) {
            val event = classify(text.toByteArray(Charsets.UTF_8))
            assertTrue("expected Application for '$text', got $event", event is IncomingChatEvent.Application)
        }
    }

    @Test
    fun `each envelope kind classifies independently`() {
        // The arms run per row; a beacon must not shadow the chat message next
        // to it in a poll batch, nor be shadowed by it.
        val kinds = listOf(
            chatEnvelope("before"),
            committerBeacon(),
            chatEnvelope("after"),
        ).map { classify(it) }

        assertTrue("row 0: ${kinds[0]}", kinds[0] is IncomingChatEvent.Application)
        assertTrue("row 1: ${kinds[1]}", kinds[1] is IncomingChatEvent.CommitterBeacon)
        assertTrue("row 2: ${kinds[2]}", kinds[2] is IncomingChatEvent.Application)
    }
}
