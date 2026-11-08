// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import me.lakko.app.chat.looksLikeCallSignaling
import me.lakko.app.chat.looksLikeGroupNameEnvelope
import me.lakko.app.chat.looksLikePinEnvelope
import me.lakko.app.chat.looksLikeProfileEnvelope
import me.lakko.app.profile.looksLikeProfileRequestEnvelope
import uniffi.lakko_ffi_uniffi.MessageBodyKind

/**
 * SERVICE-ENVELOPE classifier for the background push-preview path.
 *
 * ## The defect this closes
 *
 * Not every MLS Application message on a group journal is a chat
 * message. Three kinds are pure machine-to-machine SERVICE envelopes
 * that a human must never see:
 *
 *   1. the ADR-0042 / ADR-0044 **push-routing-secret** distribution
 *      envelope (70 bytes, domain `lakko/push-routing-secret/v1` plus a
 *      trailing NUL -- written out in words, never as a raw NUL byte,
 *      which would make git treat this file as binary),
 *   2. the **call-signaling** `CallSignalingPayload` proto — which since
 *      `docs/ux/calls.md` section 2.1 is classified rather than merely dropped:
 *      it is still never previewed as a message, but an OFFER now rings the
 *      phone instead of vanishing (see [BackgroundPayloadKind]), and
 *   3. the ADR-0045 **committer-liveness beacon** (27 bytes, domain
 *      `lakko/committer-beacon/v1` plus a trailing NUL), and
 *   4. the ADR-0048 **profile** envelope (variable length, domain
 *      `lakko/profile/v1` plus a trailing NUL). Its 17-byte prefix ending
 *      in a NUL is a complete classification on its own, so no codec is
 *      injected for it: a malformed profile envelope is still a profile
 *      envelope and is suppressed, never rendered. The background decrypt
 *      reads a well-formed one before this classifier and applies it in
 *      memory for the rest of its batch, so the sender's next row is named
 *      (`PushBackgroundDecrypt.kt`, [withBatchProfile]); it stores nothing,
 *      storing a profile is a WRITE and this path is read-only (C-59).
 *
 * Kind 3 is the MOST reachable of the three, not the least. A routing
 * secret is broadcast only on a membership change; a beacon is emitted
 * by every owner and admin every 60 s, so in an active group the newest
 * journal row -- the one the background handler previews -- is
 * frequently a beacon. Unfiltered it renders as the literal text
 * `lakko/committer-beacon/v1` plus two control bytes.
 *
 * `parseBodyEnvelope` cannot reject any of them. Its `parse_body`
 * (`crates/lakko-attachments/src/lib.rs`) carries a DELIBERATE legacy
 * fallback: bytes that are not the JSON envelope degrade to
 * `MessageBody::Chat` via `String::from_utf8_lossy`, which is what keeps
 * pre-1.19 plain-UTF-8 messages readable. Fed a service envelope that
 * same fallback happily yields "chat text" consisting of the domain
 * string plus raw binary — and the background handler rendered it as a
 * notification.
 *
 * The fix belongs HERE, not in `parse_body`: the fallback is load-bearing
 * for wire compatibility, and `lakko-attachments` does not depend on
 * `lakko-contacts`, so teaching it these domains would invert the crate
 * layering.
 *
 * ## Reuse, not a second mechanism
 *
 * The foreground `me.lakko.app.chat.ChatClient` already filters these
 * kinds correctly. This function calls that SAME code — the
 * [looksLikeCallSignaling] tag sniff and
 * [GroupPushRouting.looksLikeSecretEnvelope] — rather than restating the
 * magic bytes, so the two paths cannot drift apart.
 *
 * ## Why the two-step peek is correctness, not just speed
 *
 * The cheap peek alone is NOT a sufficient verdict for the routing
 * secret. A legacy plain-UTF-8 chat message that happens to be 70 bytes
 * long and start with `'l'` ("let's meet at ...") clears the pre-filter,
 * and suppressing it would silently swallow a real message. So the peek
 * only gates the AUTHORITATIVE, fail-closed FFI decode, exactly as the
 * foreground does; a decode failure falls through to the normal chat
 * parse. The ordering — cheap peeks first, throwing codec last — means
 * the common case (a real chat envelope, first byte `{` = 0x7B) is two
 * integer comparisons and never pays for an exception.
 *
 * ## The beacon arm differs, deliberately
 *
 * A beacon carries NO FIELDS — no epoch, no identity, no timestamp — so
 * every beacon on the wire is byte-identical, and its exact-length +
 * first-byte peek is therefore a COMPLETE test rather than merely a fast
 * path. The codec call behind it is belt-and-braces confirmation, not a
 * necessity; it stays because it is the single definition of that wire
 * format, so a version bump moves the verdict without an edit here.
 *
 * The beacon's length is a PARAMETER, never a literal in this file: it
 * comes from the FFI's `committerBeaconEnvelopeLen()`, which exists
 * precisely so three clients do not each hand-copy `27` — the drift
 * shape ADR-0044 was written about. It is injected rather than called
 * here for the same reason the codecs are: this file must stay free of
 * the UniFFI `.so` so it runs in a plain JVM unit test.
 *
 * ## Read-only
 *
 * Pure classification. No writes, no cursor advance, no persist — the
 * ADR-0041 / C-59 read-only-throwaway discipline of the background path
 * is untouched. The background path also never EMITS a beacon: a beacon
 * means "able to sweep", and it structurally cannot commit.
 *
 * @param payload the decrypted MLS Application plaintext.
 * @param decodesAsRoutingSecret the authoritative routing-secret decode
 *   (the native `decodePushRoutingSecret`, injected so this function
 *   stays free of the UniFFI `.so` and runs in a plain JVM unit test).
 *   Called ONLY after the cheap length + first-byte peek passes.
 * @param decodesAsCommitterBeacon the authoritative beacon decode (the
 *   native `decodeCommitterBeacon`), injected for the same reason.
 * @param committerBeaconEnvelopeLen the exact on-wire beacon length,
 *   read from the FFI constant at the call site — never written down.
 * @return `true` when the payload is a service envelope and must not be
 *   rendered to the user.
 */
internal fun isServiceEnvelope(
    payload: ByteArray,
    decodesAsRoutingSecret: (ByteArray) -> Boolean,
    decodesAsCommitterBeacon: (ByteArray) -> Boolean,
    committerBeaconEnvelopeLen: Int,
    decodesAsRegistrarSeed: (ByteArray) -> Boolean = { false },
    registrarSeedEnvelopeLen: Int = -1,
): Boolean = classifyBackgroundPayload(
    payload,
    decodesAsRoutingSecret,
    decodesAsCommitterBeacon,
    committerBeaconEnvelopeLen,
    decodesAsRegistrarSeed,
    registrarSeedEnvelopeLen,
) != BackgroundPayloadKind.MESSAGE

/**
 * What a decrypted background payload IS, rather than merely whether it may be
 * rendered.
 *
 * ## Why this replaced a yes/no
 *
 * [isServiceEnvelope] answered one question — "must this stay off the user's
 * screen?" — and call signaling answered yes, so the background path dropped it
 * on the floor. That is precisely why a closed app never rang: the wake arrived,
 * the offer decrypted, and the filter threw it away
 * (`docs/ux/calls.md` section 7).
 *
 * A call offer is not a message and must never be previewed as one, so the old
 * answer was right as far as it went; it just had nowhere to say "this is a
 * call". Now the same peeks produce a KIND, and the two consequences separate:
 * a service envelope is dropped, while call signaling is handed to the ring
 * ([me.lakko.app.calls.IncomingCallRinger]). [isServiceEnvelope] stays as the
 * "not a message" question, which is still exactly what the chat-preview path
 * asks.
 */
internal enum class BackgroundPayloadKind {
    /** A call offer / end / answer / candidate. Never rendered; an offer rings. */
    CALL_SIGNALING,

    /** Machine-to-machine: routing secret, committer beacon, profile, group
     *  name, profile request, shared pin, registrar seed. */
    SERVICE_ENVELOPE,

    /** Anything else — a real chat envelope, or legacy plain UTF-8 text. */
    MESSAGE,
}

/**
 * The preview kind of a body the core's `parseBodyEnvelope` classified, or
 * `null` for [MessageBodyKind.SERVICE_ENVELOPE]: a `lakko/`-domain plaintext
 * that is not JSON (an unknown envelope kind, or a known domain whose body this
 * build cannot decode, e.g. a newer profile version). It is dropped like every
 * other service envelope, so the loop falls back to the newest real message or
 * the generic banner. Pure, so the host suite pins the mapping.
 */
internal fun previewKindOf(kind: MessageBodyKind): ProcessedKind? = when (kind) {
    MessageBodyKind.CHAT -> ProcessedKind.CHAT
    MessageBodyKind.EDIT -> ProcessedKind.EDIT
    MessageBodyKind.DELETE -> ProcessedKind.DELETE
    MessageBodyKind.REACTION -> ProcessedKind.REACTION
    MessageBodyKind.TYPING -> ProcessedKind.TYPING
    MessageBodyKind.READ_RECEIPT -> ProcessedKind.READ_RECEIPT
    MessageBodyKind.SERVICE_ENVELOPE -> null
    MessageBodyKind.UNSUPPORTED -> ProcessedKind.UNSUPPORTED
}

/** Classify one decrypted MLS Application plaintext. See [isServiceEnvelope]
 *  for the parameters and the two-step-peek reasoning. */
internal fun classifyBackgroundPayload(
    payload: ByteArray,
    decodesAsRoutingSecret: (ByteArray) -> Boolean,
    decodesAsCommitterBeacon: (ByteArray) -> Boolean,
    committerBeaconEnvelopeLen: Int,
    decodesAsRegistrarSeed: (ByteArray) -> Boolean = { false },
    registrarSeedEnvelopeLen: Int = -1,
): BackgroundPayloadKind {
    // Cheap unambiguous discriminator: the CallSignalingPayload oneof
    // body tags, fields 10..18 (0x52/0x5A/0x62/0x6A, the group-control
    // 0x72/0x7A, and the two-byte 0x82 0x01 / 0x8A 0x01 / 0x92 0x01 for
    // fields 16..18, the last a 1x1 media state that never notifies — see
    // `looksLikeCallSignaling` for why the group tags also check the
    // `call_id` header). Disjoint from both the chat `{` (0x7B) and the
    // `l` (0x6C) both in-group envelope domains open with.
    if (looksLikeCallSignaling(payload)) return BackgroundPayloadKind.CALL_SIGNALING
    return if (isServiceEnvelopeOtherThanCall(
            payload,
            decodesAsRoutingSecret,
            decodesAsCommitterBeacon,
            committerBeaconEnvelopeLen,
            decodesAsRegistrarSeed,
            registrarSeedEnvelopeLen,
        )
    ) {
        BackgroundPayloadKind.SERVICE_ENVELOPE
    } else {
        BackgroundPayloadKind.MESSAGE
    }
}

private fun isServiceEnvelopeOtherThanCall(
    payload: ByteArray,
    decodesAsRoutingSecret: (ByteArray) -> Boolean,
    decodesAsCommitterBeacon: (ByteArray) -> Boolean,
    committerBeaconEnvelopeLen: Int,
    decodesAsRegistrarSeed: (ByteArray) -> Boolean = { false },
    registrarSeedEnvelopeLen: Int = -1,
): Boolean {
    // Kind 1 — cheap pre-filter (exact 70-byte length + leading `l`)
    // before the throwing codec, so a real chat message never costs an
    // exception. Authoritative + fail-closed: a retired v1 (62-byte)
    // envelope or a wrong-domain lookalike is refused and falls through
    // to the chat parse, preserving the legacy-plaintext path above.
    if (GroupPushRouting.looksLikeSecretEnvelope(payload) &&
        decodesAsRoutingSecret(payload)
    ) {
        return true
    }

    // Kind 3 — same two-step shape, but see the note above on why the
    // peek is a complete test here and the codec is confirmation.
    if (looksLikeCommitterBeacon(payload, committerBeaconEnvelopeLen) &&
        decodesAsCommitterBeacon(payload)
    ) {
        return true
    }

    // Kind 4 — ADR-0048 profile. The same prefix test the foreground
    // `ChatClient` classifier uses, so the two paths cannot drift.
    if (looksLikeProfileEnvelope(payload)) return true

    // Kind 5 — ADR-0051 group name. The same NUL-terminated-prefix shape as
    // the profile, so the peek is a complete classification here too; a
    // rename must never be previewed as a notification body reading
    // `lakko/group-name/v1`. Suppressed only — storing a name is a WRITE and
    // this path is read-only (C-59).
    if (looksLikeGroupNameEnvelope(payload)) return true

    // Kind 5b — ADR-0073 phase 2 / UX 16.5, the group-closed notice. The same
    // NUL-terminated-prefix shape as the group name (`lakko/group-closed/v1`),
    // the same foreground peek, so the two paths cannot drift; a closing must
    // never be previewed as a notification body reading the domain string.
    // Suppressed only — closing a group is a WRITE under the conversation's
    // lock, and this path is read-only (C-59); the foreground applies it when
    // it next reads the journal.
    if (me.lakko.app.chat.looksLikeGroupClosedEnvelope(payload)) return true

    // Kind 5c — ADR-0087: the group photo, the allowed reactions, the
    // conversation handle and the knock seed. NUL-terminated domains, the same
    // peek the foreground classifier uses; suppressed only (storing is a write,
    // C-59).
    if (me.lakko.app.chat.groupSettingKindOf(payload) != null) return true

    // Kind 5d — `docs/ux/who-can-find-me.md` section 6.4, a contact request
    // sent inside the group and sealed to one member. The same NUL-terminated
    // prefix peek the foreground classifier uses. Suppressed only, never a
    // notification body: opening it and adding it to Requests is the
    // foreground's (C-59), which reads the journal again when it next runs.
    if (me.lakko.app.chat.looksLikeGroupContactRequestEnvelope(payload)) return true

    // Kind 6 — ADR-0048 trigger 4, the "introduce yourselves" request. The
    // same NUL-terminated-prefix shape again, and the same foreground peek.
    // Suppressed only: ANSWERING is a publish (an MLS application message,
    // a ratchet advance), which this read-only path must never do (C-59) —
    // the foreground answers when it next reads the journal, which is when
    // the asker's request is actually read.
    if (looksLikeProfileRequestEnvelope(payload)) return true

    // Kind 7 — ADR-0052 decision 3, a shared pin / unpin. The same
    // NUL-terminated-prefix shape (`lakko/pin/v1`), the same foreground
    // peek, so the two paths cannot drift. Suppressed only, as a rename is:
    // applying a pin is a WRITE under the conversation's lock, and this
    // path is read-only (C-59); the foreground applies it when it next
    // reads the journal.
    if (looksLikePinEnvelope(payload)) return true

    // Kind 8 — ADR-0057 registrar seed, the owner appointing one member as a
    // registrar of the group's journal. The same length + first-byte peek the
    // foreground classifier uses, then the fail-closed decode. Suppressed
    // only: OPENING and storing the seed is a write under the conversation's
    // lock, which this read-only path must never do (C-59); the foreground
    // applies it when it next reads the journal.
    if (me.lakko.app.chat.looksLikeRegistrarSeedEnvelope(payload, registrarSeedEnvelopeLen, decodesAsRegistrarSeed)) {
        return true
    }

    // Kind 9 — ADR-0060 scoped discovery (introduction offer / bundle,
    // visibility, reach state, committer delegation). The same NUL-terminated-
    // prefix classification the foreground `ChatClient` uses
    // ([me.lakko.app.reach.scopedEnvelopeKindOf]), so the two paths cannot
    // drift. Suppressed only: keeping any of them is a WRITE, and this path is
    // read-only (C-59).
    if (me.lakko.app.reach.scopedEnvelopeKindOf(payload) != null) return true

    // Kind 10 — ADR-0059 section 6, a member's shared channel list. The same
    // NUL-terminated-prefix shape (`lakko/shared-channels/v1`), the same
    // foreground peek. Unfiltered it would be previewed as a notification
    // body reading the domain and the channel names. Suppressed only: filing
    // a list is a WRITE, and this path is read-only (C-59); the foreground
    // files it when it next reads the journal.
    if (me.lakko.app.discover.looksLikeSharedChannelsEnvelope(payload)) return true

    // Kind 11 — ADR-0074 part 3, the `key-rotation` envelope. It means
    // something ONLY in the account's own DeviceGroup, which this path never
    // reads; anywhere else it is dropped. The NUL-terminated-prefix peek
    // (`lakko/key-rotation/v1`) is a complete classification, and the bytes
    // behind it (the account's new entropy) are never decoded, logged or
    // previewed here.
    if (me.lakko.app.devices.looksLikeKeyRotationEnvelope(payload)) return true

    // Kind 12 — ADR-0087 section 7, a co-publisher's step-down and the
    // owner's sealed channel read key. Both ride a PublisherGroup journal,
    // which no chat path reads, but the prefix peek keeps them off every
    // preview should one ever reach a chat journal. Suppressed only (C-59).
    if (looksLikePublisherChannelEnvelope(payload)) return true

    return false
}

private val PUBLISHER_STEP_DOWN_PREFIX = "lakko/publisher-step-down/v1\u0000".toByteArray(Charsets.US_ASCII)
private val CHANNEL_READ_KEY_PREFIX = "lakko/channel-read-key/v1\u0000".toByteArray(Charsets.US_ASCII)

/** `true` for the ADR-0087 step-down and channel read-key envelopes: a
 *  NUL-terminated domain peek, no decode. */
internal fun looksLikePublisherChannelEnvelope(payload: ByteArray): Boolean =
    startsWith(payload, PUBLISHER_STEP_DOWN_PREFIX) || startsWith(payload, CHANNEL_READ_KEY_PREFIX)

private fun startsWith(payload: ByteArray, prefix: ByteArray): Boolean {
    if (payload.size < prefix.size) return false
    for (i in prefix.indices) if (payload[i] != prefix[i]) return false
    return true
}

/** First byte of the frozen 26-byte ADR-0045 domain separator
 *  `lakko/committer-beacon/v1` plus a trailing NUL (written out in
 *  words, never as a raw NUL byte). The same `'l'` every in-group
 *  envelope domain opens with, and equally frozen — it is the
 *  `incoming_envelope` dispatch prefix. */
private const val BEACON_ENVELOPE_FIRST_BYTE: Int = 0x6C

/**
 * Cheap pre-filter for an ADR-0045 committer beacon: the exact envelope
 * length plus the leading domain byte.
 *
 * [envelopeLen] is the FFI's `committerBeaconEnvelopeLen()`, threaded in
 * by the caller. Because the beacon is field-less this pair fully
 * determines the value, so unlike the routing peek it is a complete test
 * — the codec behind it confirms rather than decides. Pure —
 * host-testable.
 */
internal fun looksLikeCommitterBeacon(payload: ByteArray, envelopeLen: Int): Boolean =
    payload.size == envelopeLen &&
        payload.isNotEmpty() &&
        (payload[0].toInt() and 0xFF) == BEACON_ENVELOPE_FIRST_BYTE
