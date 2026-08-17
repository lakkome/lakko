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
 * message. Two kinds are pure machine-to-machine SERVICE envelopes that
 * a human must never see:
 *
 *   1. the ADR-0042 / ADR-0044 **push-routing-secret** distribution
 *      envelope (70 bytes, domain `lakko/push-routing-secret/v1` plus a
 *      trailing NUL -- written out in words, never as a raw NUL byte,
 *      which would make git treat this file as binary), and
 *   2. the **call-signaling** `CallSignalingPayload` proto.
 *
 * `parseBodyEnvelope` cannot reject either. Its `parse_body`
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
 * The foreground `me.lakko.app.chat.ChatClient` already filters both
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
 * ## Read-only
 *
 * Pure classification. No writes, no cursor advance, no persist — the
 * ADR-0041 / C-59 read-only-throwaway discipline of the background path
 * is untouched.
 *
 * @param payload the decrypted MLS Application plaintext.
 * @param decodesAsRoutingSecret the authoritative routing-secret decode
 *   (the native `decodePushRoutingSecret`, injected so this function
 *   stays free of the UniFFI `.so` and runs in a plain JVM unit test).
 *   Called ONLY after the cheap length + first-byte peek passes.
 * @return `true` when the payload is a service envelope and must not be
 *   rendered to the user.
 */
internal fun isServiceEnvelope(
    payload: ByteArray,
    decodesAsRoutingSecret: (ByteArray) -> Boolean,
): Boolean {
    // Cheap unambiguous discriminator: the CallSignalingPayload oneof
    // body tags (0x52/0x5A/0x62/0x6A). Disjoint from both the chat `{`
    // (0x7B) and the routing-secret `l` (0x6C).
    if (looksLikeCallSignaling(payload)) return true

    // Cheap pre-filter (exact 70-byte length + leading `l`) before the
    // throwing codec, so a real chat message never costs an exception.
    if (!GroupPushRouting.looksLikeSecretEnvelope(payload)) return false

    // Authoritative + fail-closed. A retired v1 (62-byte) envelope or a
    // wrong-domain lookalike is refused here and falls through to the
    // chat parse, preserving the legacy-plaintext path above.
    return decodesAsRoutingSecret(payload)
}
