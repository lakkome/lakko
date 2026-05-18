// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.devices

import android.graphics.Bitmap
import android.graphics.Color
import android.util.Base64
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.google.zxing.BarcodeFormat
import com.google.zxing.EncodeHintType
import com.google.zxing.MultiFormatWriter
import com.google.zxing.WriterException
import com.google.zxing.common.BitMatrix
import com.google.zxing.qrcode.decoder.ErrorCorrectionLevel

/**
 * Compose QR-image composable for the pairing payload
 * (Phase 2a3b4, mirror of
 * `apps/ios/Lakko/UI/Devices/QrImageView.swift`).
 *
 * Renders the wire-format pairing bytes — base64url-no-pad-
 * encoded as text first — into a black-on-white QR matrix
 * using **ZXing** as the encoder. The bitmap is square; the
 * caller picks the side length via [size].
 *
 * ## Why base64url + text, not raw bytes
 *
 * The wire bytes are arbitrary 0x00–0xFF; QR's `BYTE_MODE`
 * supports that, but every scanner SDK on the planet (ours
 * included — see `QrScannerComposable`) decodes to **String**
 * via UTF-8. Mixing raw bytes and `BYTE_MODE` survives a
 * round-trip only if both sides agree to skip the UTF-8 step
 * — fragile, breaks any third-party scanner the user might
 * point at the code. Going through a text encoding (base64url-
 * no-pad: ASCII alphabet, no padding) keeps the QR's
 * `ALPHANUMERIC_MODE` happy AND survives any random scanner
 * the user might use during debugging.
 *
 * iOS / web do the same — `apps/web/src/routes/Devices.tsx`
 * base64-encodes before handing to `qrcode`, and
 * `QrImageView.swift` base64-encodes before
 * `CIFilter.qrCodeGenerator`.
 *
 * ## Error correction
 *
 * `M` (medium, ~15% reconstruction) — same as iOS / web. The
 * pairing payload is small (~150 bytes encoded), so even `H`
 * would fit on screen without bloating module count to the
 * "unreadable from arm's length" point. `M` is the canonical
 * default for screen-displayed QR codes; `H` is wasted budget
 * here.
 */
@Composable
public fun QrImageComposable(
    payload: ByteArray,
    modifier: Modifier = Modifier,
    size: Dp = 240.dp,
) {
    val bitmap = remember(payload) { renderQrBitmap(payload) }
    Box(
        modifier = modifier.size(size),
        contentAlignment = Alignment.Center,
    ) {
        if (bitmap != null) {
            Image(
                bitmap = bitmap.asImageBitmap(),
                contentDescription = "Pairing QR code",
                modifier = Modifier.size(size),
            )
        } else {
            // Defensive fallback — ZXing throws only on
            // payload sizes that overflow the QR-v40 max
            // (~3000 bytes), which our ~150-byte pairing
            // payload never reaches. If we ever hit this we
            // want a visible signal, not a silent blank.
            Text(text = "QR render failed")
        }
    }
}

/**
 * Render the QR matrix to an ARGB_8888 [Bitmap]. Each ZXing
 * module → one pixel; the consumer scales via Compose
 * `Modifier.size`.
 *
 * Returns `null` on a [WriterException] (overflowed QR
 * capacity). The caller renders a one-line fallback.
 */
private fun renderQrBitmap(payload: ByteArray): Bitmap? {
    // base64url-no-pad: standard base64 then trim padding +
    // swap two URL-unsafe chars. Same alphabet the iOS / web
    // sides use; the scanner side reverses it via
    // [base64UrlDecodeNoPad].
    val text = payload.toBase64UrlNoPad()

    val hints = mapOf(
        // QR codes pinned to "M" everywhere.
        EncodeHintType.ERROR_CORRECTION to ErrorCorrectionLevel.M,
        // ZXing defaults to one-module margin; bump to 2 so
        // the printed code has a quiet zone wide enough for
        // every scanner spec.
        EncodeHintType.MARGIN to 2,
        EncodeHintType.CHARACTER_SET to Charsets.UTF_8.name(),
    )
    val matrix: BitMatrix = try {
        MultiFormatWriter().encode(
            text,
            BarcodeFormat.QR_CODE,
            0,
            0,
            hints,
        )
    } catch (_: WriterException) {
        return null
    }
    val width = matrix.width
    val height = matrix.height
    val pixels = IntArray(width * height)
    for (y in 0 until height) {
        val rowOffset = y * width
        for (x in 0 until width) {
            pixels[rowOffset + x] = if (matrix[x, y]) Color.BLACK else Color.WHITE
        }
    }
    val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
    bitmap.setPixels(pixels, 0, width, 0, 0, width, height)
    return bitmap
}

private fun ByteArray.toBase64UrlNoPad(): String =
    Base64.encodeToString(this, Base64.URL_SAFE or Base64.NO_PADDING or Base64.NO_WRAP)
