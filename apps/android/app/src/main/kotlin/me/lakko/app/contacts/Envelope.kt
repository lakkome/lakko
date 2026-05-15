// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

/**
 * One-byte kind discriminator for messages a Lakko mailbox
 * carries.
 *
 * Mirror of `apps/web/src/contacts/envelope.ts`. Three kinds today:
 *
 *   * [KIND_CONTACT_REQUEST] (`0x01`) — body is a
 *     `ContactRequest` wire blob produced by
 *     [uniffi.lakko_ffi_uniffi.composeContactRequest].
 *   * [KIND_WELCOME] (`0x02`) — body is an MLS Welcome wire
 *     blob produced by `Group.addMember(...)`'s `welcome` field
 *     (consumed by the chat surface in 1.15f).
 *   * [KIND_CONTACT_ACK] (`0x03`) — body is a `ContactAck` wire
 *     blob (1.9b1) produced by
 *     [uniffi.lakko_ffi_uniffi.composeContactAck], the
 *     recipient's accept-reply to the original ContactRequest's
 *     sender.
 *
 * ## Why a 1-byte discriminator?
 *
 * The DS server is opaque to mailbox payloads (publishes raw
 * bytes); the receiver needs to know which decoder to reach for
 * before handing the body to UniFFI. A single byte is enough —
 * the inner formats already carry their own integrity
 * (`ContactRequest` is Ed25519-signed; MLS `Welcome` is
 * authenticated by the sender's KeyPackage signature). A flipped
 * kind byte just makes the receiver dispatch to the wrong
 * decoder, which then rejects the body cleanly.
 *
 * The wrapping happens **here** on the Kotlin side rather than
 * inside `lakko-contacts` because `Welcome` is an MLS concept
 * owned by `lakko-mls`, while `ContactRequest` is owned by
 * `lakko-contacts` — neither is the natural home for a
 * cross-format envelope. When the iOS client comes online we
 * lift the constants into a shared Rust crate.
 */

/** Discriminator: body is a `ContactRequest` wire blob. */
public const val KIND_CONTACT_REQUEST: Byte = 0x01

/** Discriminator: body is an MLS Welcome wire blob (1.15f). */
public const val KIND_WELCOME: Byte = 0x02

/** Discriminator: body is a `ContactAck` wire blob (1.9b1 —
 *  recipient's accept-reply to the original ContactRequest's
 *  sender). */
public const val KIND_CONTACT_ACK: Byte = 0x03

/** Wrap inner bytes with the given kind discriminator. The
 *  returned [ByteArray] is fresh — caller may mutate. */
public fun wrapEnvelope(kind: Byte, body: ByteArray): ByteArray {
    require(
        kind == KIND_CONTACT_REQUEST ||
            kind == KIND_WELCOME ||
            kind == KIND_CONTACT_ACK,
    ) {
        "unknown envelope kind: $kind"
    }
    val out = ByteArray(body.size + 1)
    out[0] = kind
    body.copyInto(out, destinationOffset = 1)
    return out
}

/** Discriminated parse output. Unknown kinds surface so the
 *  caller can log + skip rather than throw. */
public sealed interface ParsedEnvelope {
    public data class ContactRequest(val body: ByteArray) : ParsedEnvelope {
        override fun equals(other: Any?): Boolean =
            other is ContactRequest && body.contentEquals(other.body)
        override fun hashCode(): Int = body.contentHashCode()
    }
    public data class Welcome(val body: ByteArray) : ParsedEnvelope {
        override fun equals(other: Any?): Boolean =
            other is Welcome && body.contentEquals(other.body)
        override fun hashCode(): Int = body.contentHashCode()
    }
    public data class ContactAck(val body: ByteArray) : ParsedEnvelope {
        override fun equals(other: Any?): Boolean =
            other is ContactAck && body.contentEquals(other.body)
        override fun hashCode(): Int = body.contentHashCode()
    }
    /** Future / unknown kind byte. Caller logs + advances cursor. */
    public data class Unknown(val rawKind: Int, val body: ByteArray) : ParsedEnvelope {
        override fun equals(other: Any?): Boolean =
            other is Unknown && rawKind == other.rawKind && body.contentEquals(other.body)
        override fun hashCode(): Int = rawKind.hashCode() * 31 + body.contentHashCode()
    }
}

/** Strip the leading kind byte and dispatch. Empty input
 *  surfaces as [ParsedEnvelope.Unknown] with `rawKind = -1`
 *  rather than throwing — Phase-1 the DS doesn't lower-bound
 *  payload size on read, and we want garbage to skip-and-log
 *  on the receiver, not crash. */
public fun parseEnvelope(bytes: ByteArray): ParsedEnvelope {
    if (bytes.isEmpty()) {
        return ParsedEnvelope.Unknown(rawKind = -1, body = bytes)
    }
    val kind = bytes[0]
    // `body` is a fresh copy — Kotlin doesn't have ZeroCopy
    // subarrays the way Solid's TS port does via `.subarray`.
    // The extra alloc is acceptable on the receiver path.
    val body = bytes.copyOfRange(1, bytes.size)
    return when (kind) {
        KIND_CONTACT_REQUEST -> ParsedEnvelope.ContactRequest(body)
        KIND_WELCOME -> ParsedEnvelope.Welcome(body)
        KIND_CONTACT_ACK -> ParsedEnvelope.ContactAck(body)
        else -> ParsedEnvelope.Unknown(rawKind = kind.toInt() and 0xFF, body = body)
    }
}
