// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.moderation

import java.util.Base64

/*
 * ADR-0089 section 1 — attested chat messages.
 *
 * A chat plaintext is `lakko/attested-message/v1\0 || 0x01 || sig(64) ||
 * envelope`, signed by the sending leaf over the group id and the envelope.
 * The core does the signing, the split and the check (`attestMessage`,
 * `unwrapAttested`, `messageAttestation`); this file holds only the cheap
 * prefix peek that keeps a plain plaintext off the native core, and the
 * record a received message keeps for a report's evidence.
 */

/** The wrapper's domain, the same bytes as `ATTESTED_DOMAIN` in `lakko-attachments`. */
internal val ATTESTED_DOMAIN: ByteArray = "lakko/attested-message/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0)

/** Domain, version byte and signature. */
internal const val ATTESTED_OVERHEAD: Int = 26 + 1 + 64

/**
 * Whether [bytes] open with the attested wrapper's domain. A peek only: the
 * core decides whether the wrapper is well formed, and a malformed one stays
 * whole after `unwrapAttested` and classifies as a service envelope.
 */
public fun looksAttested(bytes: ByteArray): Boolean {
    if (bytes.size < ATTESTED_DOMAIN.size) return false
    for (i in ATTESTED_DOMAIN.indices) if (bytes[i] != ATTESTED_DOMAIN[i]) return false
    return true
}

/** The envelope inside an attested plaintext, or [bytes] unchanged. */
public fun unwrapForBody(
    bytes: ByteArray,
    unwrap: (ByteArray) -> ByteArray = { uniffi.lakko_ffi_uniffi.unwrapAttested(it) },
): ByteArray = if (looksAttested(bytes)) unwrap(bytes) else bytes

/** What the signature of a received message showed, as stored. */
public enum class AttestationVerdict(public val wire: String) {
    VERIFIED("verified"),
    FAILED("failed"),
    ABSENT("absent"),
    ;

    public companion object {
        public fun fromWire(value: String?): AttestationVerdict? = entries.firstOrNull { it.wire == value }
    }
}

/**
 * What a received message from someone else keeps for a report, captured at
 * receipt: the leaf may be reused later (C-68), so none of this is resolved
 * again afterwards. Base64 so the record compares by value.
 */
public data class MessageEvidence(
    val verdict: AttestationVerdict,
    /** The decrypted plaintext, wrapper included (standard base64). */
    val plaintextB64: String,
    /** The sender leaf's 32-byte signature key (standard base64). */
    val leafKeyB64: String,
    /** The leaf's device binding (`Group.deviceBindingAt`), or `null`. */
    val bindingB64: String?,
) {
    public val plaintext: ByteArray get() = Base64.getDecoder().decode(plaintextB64)
    public val leafKey: ByteArray get() = Base64.getDecoder().decode(leafKeyB64)
    public val binding: ByteArray get() = bindingB64?.let { Base64.getDecoder().decode(it) } ?: ByteArray(0)

    public companion object {
        public fun of(
            verdict: AttestationVerdict,
            plaintext: ByteArray,
            leafKey: ByteArray,
            binding: ByteArray?,
        ): MessageEvidence = MessageEvidence(
            verdict = verdict,
            plaintextB64 = Base64.getEncoder().encodeToString(plaintext),
            leafKeyB64 = Base64.getEncoder().encodeToString(leafKey),
            bindingB64 = binding?.let { Base64.getEncoder().encodeToString(it) },
        )
    }
}

/**
 * The evidence of one received application message, read at receipt from the
 * live group: the sender leaf's own key, the signature verdict over it, and
 * the leaf's device binding. `null` when the leaf cannot be resolved (an
 * external sender, a torn roster read): such a message carries no evidence
 * and is left out of a report.
 */
public fun receivedEvidence(
    group: uniffi.lakko_ffi_uniffi.Group,
    plaintext: ByteArray,
    senderLeaf: UInt?,
): MessageEvidence? {
    val leaf = senderLeaf ?: return null
    return try {
        val groupId = group.groupId()
        val leafKey = group.members().firstOrNull { it.leafIndex == leaf }?.signaturePublicKey ?: return null
        val verdict = when (uniffi.lakko_ffi_uniffi.messageAttestation(plaintext, groupId, leafKey)) {
            uniffi.lakko_ffi_uniffi.FfiAttestation.VERIFIED -> AttestationVerdict.VERIFIED
            uniffi.lakko_ffi_uniffi.FfiAttestation.FAILED -> AttestationVerdict.FAILED
            uniffi.lakko_ffi_uniffi.FfiAttestation.ABSENT -> AttestationVerdict.ABSENT
        }
        val binding = runCatching { group.deviceBindingAt(leaf) }.getOrNull()
        MessageEvidence.of(verdict, plaintext, leafKey, binding)
    } catch (_: Throwable) {
        null
    }
}
