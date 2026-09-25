// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui

import android.graphics.Bitmap
import android.graphics.Canvas
import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.AndroidComposeTestRule

/**
 * The pixels a composition actually drew, for the assertions the semantics tree
 * cannot make.
 *
 * Most UI assertions should be about semantics or about measured bounds: they
 * survive a colour change, a theme, a font. A few cannot be — whether a
 * `Canvas` drew bars of differing heights or one flat line is a question only
 * the pixels answer — and this is for those.
 *
 * `compose.ui.test`'s own `captureToImage()` is not available here: it forces a
 * redraw through a real display pipeline and times out under Robolectric. What
 * does work is asking the laid-out view hierarchy to draw itself into a bitmap,
 * which Robolectric's NATIVE graphics mode renders for real — so every test
 * using this must carry `@GraphicsMode(GraphicsMode.Mode.NATIVE)`, or it will
 * read a blank one.
 */
public class RootPixels(private val bitmap: Bitmap) {

    public val width: Int get() = bitmap.width
    public val height: Int get() = bitmap.height

    /** The ARGB pixel at [x], [y] in WINDOW coordinates. */
    public fun argbAt(x: Int, y: Int): Int = bitmap.getPixel(x, y)

    /** The red channel at [x], [y], 0..255. */
    public fun redAt(x: Int, y: Int): Int = argbAt(x, y) shr 16 and 0xFF
}

/**
 * Draw the whole window into a bitmap and hand back its pixels, in the same
 * coordinate space as a semantics node's `boundsInWindow`.
 */
public fun AndroidComposeTestRule<*, out ComponentActivity>.captureRootPixels(): RootPixels {
    waitForIdle()
    val view = activity.window.decorView
    check(view.width > 0 && view.height > 0) {
        "the window was never laid out (${view.width}x${view.height}), so there is nothing to read"
    }
    val bitmap = Bitmap.createBitmap(view.width, view.height, Bitmap.Config.ARGB_8888)
    view.draw(Canvas(bitmap))
    return RootPixels(bitmap)
}
