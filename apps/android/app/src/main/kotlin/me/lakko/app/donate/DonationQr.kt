// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.donate

import com.google.zxing.EncodeHintType
import com.google.zxing.WriterException
import com.google.zxing.qrcode.decoder.ErrorCorrectionLevel
import com.google.zxing.qrcode.encoder.Encoder

/**
 * The QR of a crypto address (docs/ux/donations.md section 2.4): error
 * correction H, a quiet zone of four modules, square modules, and, when the
 * signed `icon` names a coin, a clear square in the centre where the coin
 * mark is drawn. Never the Lakko logo: this is not the device-pairing
 * branded QR. Kept apart from Compose so it runs on a host JVM.
 */
public object DonationQr {
    /** The quiet zone, in modules, on every side. */
    public const val QUIET_MODULES: Int = 4

    /** The centre square cleared for a mark: first row / column and span,
     *  in modules (without the quiet zone). */
    public class Hole(public val from: Int, public val span: Int)

    public class Plan(
        /** Modules per side, without the quiet zone. */
        public val size: Int,
        /** Row-major, `true` = dark, already cleared under [hole]. */
        public val dark: BooleanArray,
        public val hole: Hole?,
    ) {
        /** Modules per side with the quiet zone. */
        public val sideModules: Int get() = size + 2 * QUIET_MODULES

        /** Whether module ([x], [y]) is dark (quiet zone included). */
        public fun isDark(x: Int, y: Int): Boolean {
            val mx = x - QUIET_MODULES
            val my = y - QUIET_MODULES
            if (mx !in 0 until size || my !in 0 until size) return false
            return dark[my * size + mx]
        }
    }

    /** The span cleared for a mark on a code of [size] modules: the largest
     *  odd number not above 20% of the side (under 7% of the data area,
     *  well inside what level H restores), or 0 when that is under 5. */
    public fun holeSpan(size: Int): Int {
        var k = (size * 0.2).toInt()
        if (k % 2 == 0) k -= 1
        return if (k >= 5) k else 0
    }

    /** Encode [address] at level H; with [withMark] the centre is cleared.
     *  `null` when it does not fit a QR code. */
    public fun plan(address: String, withMark: Boolean): Plan? {
        val hints = if (address.all { it.code < 0x80 }) {
            emptyMap()
        } else {
            mapOf(EncodeHintType.CHARACTER_SET to Charsets.UTF_8.name())
        }
        val matrix = try {
            Encoder.encode(address, ErrorCorrectionLevel.H, hints).matrix
        } catch (_: WriterException) {
            return null
        }
        val n = matrix.width
        val dark = BooleanArray(n * n) { i -> matrix.get(i % n, i / n).toInt() == 1 }
        val span = if (withMark) holeSpan(n) else 0
        val hole = if (span == 0) null else Hole((n - span) / 2, span)
        if (hole != null) {
            for (y in hole.from until hole.from + hole.span) {
                for (x in hole.from until hole.from + hole.span) dark[y * n + x] = false
            }
        }
        return Plan(n, dark, hole)
    }
}
