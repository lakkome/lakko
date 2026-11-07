// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.devices

import android.graphics.BitmapFactory
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Canvas
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.drawscope.CanvasDrawScope
import androidx.compose.ui.graphics.drawscope.translate
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp
import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.NotFoundException
import com.google.zxing.RGBLuminanceSource
import com.google.zxing.common.HybridBinarizer
import com.google.zxing.qrcode.QRCodeReader
import com.google.zxing.qrcode.decoder.ErrorCorrectionLevel
import java.io.File
import java.util.Base64
import kotlin.random.Random
import me.lakko.app.R
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.GraphicsMode

/**
 * The branded QR code scans (design system section 3, "QR code"): the
 * Compose drawing [BrandedQr.paint] makes, with the real brand mark
 * (`lakko_brand_mark`) in the badge, rendered by Robolectric's native
 * graphics and read back by ZXing's QR reader (the encoder's own library,
 * already a dependency for the scanner).
 *
 * The host JVM loads no native core, so the pairing payload is not built by
 * `buildPairingQr` here: it is 626 seeded random bytes, the length of the
 * largest payload measured through the real encoder (a key package and a
 * device card of maximal length), base64url-encoded to 835 characters as
 * [QrImageComposable] does.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class BrandedQrTest {

    private val invite = "https://lakko.me/i/AbCdEfGh#0123456789abcdef"
    private val otpauth =
        "otpauth://totp/Lakko:valentin_goncharov?secret=JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP" +
            "&issuer=Lakko&algorithm=SHA1&digits=6&period=30"
    private val threshold = "h".repeat(BrandedQr.LOGO_MAX_BYTES)
    private val pairing: String = Base64.getUrlEncoder().withoutPadding()
        .encodeToString(Random(626).nextBytes(626))

    private fun mark(): ImageBitmap =
        BitmapFactory.decodeResource(
            RuntimeEnvironment.getApplication().resources,
            R.drawable.lakko_brand_mark,
        ).asImageBitmap()

    /** Draw [text] at [sideDp] on a dark page, at [scale] pixels per dp. */
    private fun render(text: String, sideDp: Int, scale: Float): ImageBitmap {
        val plan = requireNotNull(BrandedQr.plan(text))
        val side = BrandedQr.side(plan, sideDp.dp).value * scale
        val pad = 24f * scale
        val total = (side + 2 * pad).toInt()
        val image = ImageBitmap(total, total)
        CanvasDrawScope().draw(
            Density(scale),
            LayoutDirection.Ltr,
            Canvas(image),
            Size(total.toFloat(), total.toFloat()),
        ) {
            drawRect(Color(0xFF0F1220))
            translate(pad, pad) {
                with(BrandedQr) {
                    paint(plan, side, cardRadiusPx(scale), if (plan.logo != null) mark() else null)
                }
            }
        }
        return image
    }

    private fun decode(image: ImageBitmap): String? {
        val bitmap = image.asAndroidBitmap()
        val pixels = IntArray(bitmap.width * bitmap.height)
        bitmap.getPixels(pixels, 0, bitmap.width, 0, 0, bitmap.width, bitmap.height)
        val source = RGBLuminanceSource(bitmap.width, bitmap.height, pixels)
        return try {
            QRCodeReader().decode(
                BinaryBitmap(HybridBinarizer(source)),
                mapOf(DecodeHintType.TRY_HARDER to true),
            ).text
        } catch (_: NotFoundException) {
            null
        } catch (_: com.google.zxing.ChecksumException) {
            null
        } catch (_: com.google.zxing.FormatException) {
            null
        }
    }

    /** With the `lakko.qrDumpDir` system property set, keep a rendering. */
    private fun dump(name: String, image: ImageBitmap) {
        val dir = System.getProperty("lakko.qrDumpDir") ?: System.getenv("LAKKO_QR_DUMP_DIR") ?: return
        File(dir, "$name.png").outputStream().use {
            image.asAndroidBitmap().compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it)
        }
    }

    private fun assertScans(name: String, text: String, sideDp: Int, scale: Float) {
        val image = render(text, sideDp, scale)
        dump("android-$name-${sideDp}dp-${scale}x", image)
        assertEquals("$name at ${sideDp}dp, ${scale}x", text, decode(image))
    }

    @Test
    fun theLargestPairingPayloadHasNoLogoAndLevelM() {
        assertEquals(835, pairing.length)
        val plan = requireNotNull(BrandedQr.plan(pairing))
        assertNull(plan.logo)
        assertEquals(ErrorCorrectionLevel.M, plan.level)
        assertEquals(109, plan.size)
        assertEquals(BrandedQr.MinSideNoLogo, BrandedQr.side(plan, 0.dp))
    }

    @Test
    fun shortTextsGetALogoAtLevelH() {
        for (text in listOf(invite, otpauth, threshold)) {
            val plan = requireNotNull(BrandedQr.plan(text))
            assertEquals(ErrorCorrectionLevel.H, plan.level)
            assertNotNull(plan.logo)
            val logo = plan.logo!!
            assertTrue(logo.span <= plan.size * 0.2)
            assertEquals(1, logo.span % 2)
        }
        assertNull(BrandedQr.plan(threshold + "h")!!.logo)
        // The same symbols as the web and iOS encoders: version 5 (37
        // modules) for the invite link, version 15 (77) at the threshold.
        assertEquals(37, BrandedQr.plan(invite)!!.size)
        assertEquals(77, BrandedQr.plan(threshold)!!.size)
    }

    @Test
    fun thePairingPayloadScans() {
        assertScans("pairing", pairing, 288, 1f)
        assertScans("pairing", pairing, 288, 3f)
    }

    @Test
    fun anInviteLinkScans() {
        assertScans("invite", invite, 240, 1f)
        assertScans("invite", invite, 264, 3f)
    }

    @Test
    fun theSetupCodeScans() {
        assertScans("otpauth", otpauth, 240, 2f)
    }

    @Test
    fun aTextAtTheLogoThresholdScans() {
        assertScans("threshold", threshold, 240, 1f)
        assertScans("threshold", threshold, 240, 3f)
    }
}
