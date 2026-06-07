// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

/**
 * DeviceGroup journal envelope (G-13, ADR-0015 sprint 5).
 *
 * Every DeviceGroup DS-journal entry is wrapped so a Remove commit can carry
 * the peer-verification attestation alongside the MLS message it belongs to.
 * Peer devices unwrap it and feed `(message, attestation)` to
 * `DeviceGroup.processIncomingVerifyingRemovals`, which refuses a removal the
 * group's policy requires to be attested but isn't.
 *
 * Layout (big-endian):
 *
 *     version          : Byte = 1
 *     attestation_len  : UInt32
 *     attestation      : attestation_len bytes   (0 = none)
 *     message          : the remaining bytes      (MLS wire message)
 *
 * Mirror of `apps/web/src/devices/journal-envelope.ts`. Non-remove entries
 * carry `attestation_len = 0`. No raw fallback — the journal is pre-launch.
 */
internal object DeviceGroupJournalEnvelope {
    private const val VERSION: Byte = 1
    private const val HEADER_LEN = 5

    /** Wrap an MLS message plus an optional removal attestation. */
    fun encode(message: ByteArray, attestation: ByteArray?): ByteArray {
        val att = attestation ?: ByteArray(0)
        val out = ByteArray(HEADER_LEN + att.size + message.size)
        out[0] = VERSION
        out[1] = ((att.size ushr 24) and 0xff).toByte()
        out[2] = ((att.size ushr 16) and 0xff).toByte()
        out[3] = ((att.size ushr 8) and 0xff).toByte()
        out[4] = (att.size and 0xff).toByte()
        att.copyInto(out, HEADER_LEN)
        message.copyInto(out, HEADER_LEN + att.size)
        return out
    }

    /**
     * Unwrap an envelope produced by [encode]. Returns `(message,
     * attestation)`; the attestation is `null` when the entry carries none.
     *
     * @throws IllegalArgumentException on a short / bad-version / overflowing
     *   envelope.
     */
    fun decode(bytes: ByteArray): Pair<ByteArray, ByteArray?> {
        require(bytes.size >= HEADER_LEN) { "DeviceGroup journal entry too short" }
        require(bytes[0] == VERSION) {
            "unsupported DeviceGroup journal envelope version ${bytes[0]}"
        }
        val attLen =
            ((bytes[1].toInt() and 0xff) shl 24) or
                ((bytes[2].toInt() and 0xff) shl 16) or
                ((bytes[3].toInt() and 0xff) shl 8) or
                (bytes[4].toInt() and 0xff)
        val total = HEADER_LEN + attLen
        require(attLen >= 0 && total <= bytes.size) {
            "DeviceGroup journal entry attestation length overflows"
        }
        val attestation = if (attLen > 0) bytes.copyOfRange(HEADER_LEN, total) else null
        val message = bytes.copyOfRange(total, bytes.size)
        return message to attestation
    }
}
