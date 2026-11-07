// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.devices

import android.util.Base64
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import me.lakko.app.l10n.t

/**
 * The pairing payload as a branded QR code (design system section 3, "QR
 * code"; drawn by [BrandedQrImage], mirror of
 * `apps/ios/Lakko/UI/Devices/QrImageView.swift`).
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
 * no-pad: ASCII alphabet, no padding) survives any scanner.
 *
 * iOS / web do the same — `apps/web/src/routes/Devices.tsx`
 * and `QrImageView.swift` base64-encode before encoding.
 *
 * [size] is the requested card side, raised to the design-system minimum;
 * the white card and its quiet zone are part of the composable.
 */
@Composable
public fun QrImageComposable(
    payload: ByteArray,
    modifier: Modifier = Modifier,
    size: Dp = 288.dp,
) {
    val text = remember(payload) { payload.toBase64UrlNoPad() }
    BrandedQrImage(
        text = text,
        size = size,
        contentDescription = t("devices.pairing.qr_description"),
        modifier = modifier,
    )
}

/**
 * Render a RAW-TEXT QR (no base64 wrapping) — for payloads that are
 * already text a third-party scanner must read verbatim, e.g. an invite
 * link or the `otpauth://` provisioning URI an authenticator app consumes
 * (ADR-0018 factor (c)). The pairing flow keeps the byte + base64url path
 * above.
 */
@Composable
public fun QrTextImageComposable(
    text: String,
    modifier: Modifier = Modifier,
    size: Dp = 240.dp,
) {
    BrandedQrImage(
        text = text,
        size = size,
        contentDescription = t("common.qr_code"),
        modifier = modifier,
    )
}

private fun ByteArray.toBase64UrlNoPad(): String =
    Base64.encodeToString(this, Base64.URL_SAFE or Base64.NO_PADDING or Base64.NO_WRAP)
