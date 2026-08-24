// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import me.lakko.app.chat.looksLikeCallSignaling

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
 *   2. the **call-signaling** `CallSignalingPayload` proto, and
 *   3. the ADR-0045 **committer-liveness beacon** (27 bytes, domain
 *      `lakko/committer-beacon/v1` plus a trailing NUL).
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
): Boolean {
    // Cheap unambiguous discriminator: the CallSignalingPayload oneof
    // body tags (0x52/0x5A/0x62/0x6A). Disjoint from both the chat `{`
    // (0x7B) and the `l` (0x6C) both in-group envelope domains open with.
    if (looksLikeCallSignaling(payload)) return true

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

    return false
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
