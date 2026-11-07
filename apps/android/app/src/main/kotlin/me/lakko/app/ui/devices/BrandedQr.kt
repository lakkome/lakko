// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.devices

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.FilterQuality
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.res.imageResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import com.google.zxing.EncodeHintType
import com.google.zxing.WriterException
import com.google.zxing.qrcode.decoder.ErrorCorrectionLevel
import com.google.zxing.qrcode.encoder.Encoder
import me.lakko.app.R
import me.lakko.app.l10n.t
import kotlin.math.roundToInt

/**
 * The branded QR code (design system section 3, "QR code"), the same rules
 * as the web `ui/branded-qr.ts` and iOS `UI/Devices/BrandedQr.swift`: ZXing
 * encodes, the app draws the modules itself.
 *
 * The encoded text is never changed by the styling; only the error
 * correction level depends on whether a logo is drawn.
 */
internal object BrandedQr {
    /** A logo is drawn only for a text of at most this many UTF-8 bytes: it
     *  then fits QR version 15 at level H in byte mode on every encoder. */
    const val LOGO_MAX_BYTES: Int = 220

    /** The quiet zone, in modules, on every side of the code. */
    const val QUIET_MODULES: Int = 4

    /** The smallest card side for a code with a logo, and for one without. */
    val MinSide: Dp = 240.dp
    val MinSideNoLogo: Dp = 288.dp
    private val CardRadius: Dp = 12.dp

    /** Fixed colours: the card is always light, whatever the theme. */
    val CardColor: Color = Color(0xFFFFFFFF)
    val DataInk: Color = Color(0xFF312E81)
    val FinderInk: Color = Color(0xFF4338CA)

    private const val DOT_DIAMETER = 1.0f
    private const val BADGE_RADIUS = 0.22f
    private const val MARK_SIDE = 0.76f

    /** The badge, in modules: first row / column covered and the span. */
    class Logo(val from: Int, val span: Int)

    class Plan(
        /** Modules per side, without the quiet zone. */
        val size: Int,
        /** Row-major, `true` = dark. */
        val dark: BooleanArray,
        val level: ErrorCorrectionLevel,
        val logo: Logo?,
    )

    /** The badge span for a code of [size] modules: the largest odd number
     *  not above 20% of the side, or 0 when that is under 5. */
    fun logoSpan(size: Int): Int {
        var k = (size * 0.2).toInt()
        if (k % 2 == 0) k -= 1
        return if (k >= 5) k else 0
    }

    /** Encode [text] and decide the level and the logo; `null` when the text
     *  does not fit a QR code. */
    fun plan(text: String): Plan? {
        val wantsLogo = text.toByteArray(Charsets.UTF_8).size <= LOGO_MAX_BYTES
        val level = if (wantsLogo) ErrorCorrectionLevel.H else ErrorCorrectionLevel.M
        // ASCII text is encoded without a charset hint, so ZXing adds no ECI
        // header and makes the same symbol as the web and iOS encoders; other
        // text keeps UTF-8.
        val hints = if (text.all { it.code < 0x80 }) {
            emptyMap()
        } else {
            mapOf(EncodeHintType.CHARACTER_SET to Charsets.UTF_8.name())
        }
        val matrix = try {
            Encoder.encode(text, level, hints).matrix
        } catch (_: WriterException) {
            return null
        }
        val n = matrix.width
        val dark = BooleanArray(n * n) { i -> matrix.get(i % n, i / n).toInt() == 1 }
        val span = if (wantsLogo) logoSpan(n) else 0
        val logo = if (span == 0) null else Logo((n - span) / 2, span)
        return Plan(n, dark, level, logo)
    }

    /** The card side a caller's [requested] side becomes. */
    fun side(plan: Plan, requested: Dp): Dp {
        val min = if (plan.logo == null) MinSideNoLogo else MinSide
        return if (requested > min) requested else min
    }

    /** The card's corner radius in pixels, at [dpPx] pixels per dp. */
    fun cardRadiusPx(dpPx: Float): Float = CardRadius.value * dpPx

    private fun inFinder(x: Int, y: Int, n: Int): Boolean =
        (x < 7 && y < 7) || (x >= n - 7 && y < 7) || (x < 7 && y >= n - 7)

    private fun DrawScope.roundRect(x: Float, y: Float, w: Float, h: Float, r: Float, color: Color) {
        val rr = minOf(r, w / 2, h / 2)
        drawRoundRect(color, Offset(x, y), Size(w, h), CornerRadius(rr, rr))
    }

    /** Draw [plan] on a card of [side] pixels at the scope's origin;
     *  [cardRadius] in pixels. */
    fun DrawScope.paint(plan: Plan, side: Float, cardRadius: Float, mark: ImageBitmap?) {
        val n = plan.size
        val m = side / (n + 2 * QUIET_MODULES)
        fun at(module: Int): Float = (module + QUIET_MODULES) * m

        roundRect(0f, 0f, side, side, cardRadius, CardColor)

        for ((fx, fy) in listOf(0 to 0, (n - 7) to 0, 0 to (n - 7))) {
            val ox = at(fx)
            val oy = at(fy)
            roundRect(ox, oy, 7 * m, 7 * m, m, FinderInk)
            roundRect(ox + m, oy + m, 5 * m, 5 * m, m / 2, CardColor)
            roundRect(ox + 2 * m, oy + 2 * m, 3 * m, 3 * m, m / 2, FinderInk)
        }

        val logo = plan.logo
        val r = DOT_DIAMETER * m / 2
        val dots = Path()
        for (y in 0 until n) {
            for (x in 0 until n) {
                if (!plan.dark[y * n + x] || inFinder(x, y, n)) continue
                if (logo != null &&
                    x in logo.from until logo.from + logo.span &&
                    y in logo.from until logo.from + logo.span
                ) {
                    continue
                }
                val cx = at(x) + m / 2
                val cy = at(y) + m / 2
                dots.addOval(Rect(cx - r, cy - r, cx + r, cy + r))
            }
        }
        drawPath(dots, DataInk)

        if (logo != null) {
            val bx = at(logo.from)
            val badge = logo.span * m
            roundRect(bx, bx, badge, badge, BADGE_RADIUS * badge, CardColor)
            if (mark != null) {
                val s = MARK_SIDE * badge
                val o = bx + (badge - s) / 2
                drawImage(
                    image = mark,
                    srcOffset = IntOffset.Zero,
                    srcSize = IntSize(mark.width, mark.height),
                    dstOffset = IntOffset(o.roundToInt(), o.roundToInt()),
                    dstSize = IntSize(s.roundToInt(), s.roundToInt()),
                    filterQuality = FilterQuality.High,
                )
            }
        }
    }
}

/** One branded QR code for [text]: [size] is the requested card side,
 *  raised to the design-system minimum; the white card and its quiet zone
 *  are part of it. */
@Composable
internal fun BrandedQrImage(
    text: String,
    size: Dp,
    contentDescription: String,
    modifier: Modifier = Modifier,
) {
    val plan = remember(text) { BrandedQr.plan(text) }
    if (plan == null) {
        Text(text = t("common.error.qr_render"))
        return
    }
    val mark = ImageBitmap.imageResource(R.drawable.lakko_brand_mark)
    Canvas(
        modifier = modifier
            .size(BrandedQr.side(plan, size))
            .semantics {
                this.contentDescription = contentDescription
                role = Role.Image
            },
    ) {
        // `this.size`: the scope's size, not the `size` parameter.
        val side = this.size.minDimension
        with(BrandedQr) {
            paint(plan, side, cardRadiusPx(density), if (plan.logo != null) mark else null)
        }
    }
}
