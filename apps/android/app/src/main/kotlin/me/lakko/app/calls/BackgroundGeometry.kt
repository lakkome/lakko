// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * The coordinate maps of the camera background compositor (ADR-0089
 * section 2), as plain arithmetic so the host tests can check them.
 *
 * Three spaces, all in texture coordinates from 0 to 1 with `y` up (the GL
 * convention libwebrtc uses for texture frames):
 *
 *  - **buffer**: the camera frame as it is stored, before its rotation;
 *    the compositor draws its output in this space, so the frame keeps its
 *    size and rotation and only its pixels change.
 *  - **upright**: the picture as it is shown, the buffer turned by the
 *    frame's rotation. libwebrtc's `VideoFrameDrawer` samples the buffer at
 *    `T(0.5) R(rotation) T(-0.5)` of the upright point; the same matrix is
 *    used here, so "upright" means what every renderer shows.
 *  - **model**: the 256 x 144 picture the segmentation model takes, the
 *    upright picture fitted inside it by the core's rectangle ([ModelFit]:
 *    a phone held upright gives a tall picture in a wide input), with its
 *    first row the TOP of the picture: the model reads rows top to bottom,
 *    and `glReadPixels` returns the window's bottom row first, so the model
 *    pass draws the picture upside down in GL terms. The mask comes back in
 *    the same layout.
 *
 * A picture background (a built-in or an own photo) is in **picture**
 * space: its pixel rows top to bottom, cropped by the cover rectangle the
 * Rust core computes for the upright frame size.
 *
 * Matrices are 3 x 3 affine maps in column-major order, as
 * `glUniformMatrix3fv` takes them.
 */
internal class Affine(
    /** Column-major: `[a, c, 0, b, d, 0, tx, ty, 1]` for
     *  `x' = a x + b y + tx`, `y' = c x + d y + ty`. */
    val m: FloatArray,
) {
    fun apply(x: Float, y: Float): Pair<Float, Float> =
        (m[0] * x + m[3] * y + m[6]) to (m[1] * x + m[4] * y + m[7])

    /** `this` after [first]: the map that applies [first], then `this`. */
    fun after(first: Affine): Affine {
        val a = m
        val b = first.m
        val out = FloatArray(9)
        for (col in 0 until 3) {
            for (row in 0 until 3) {
                var sum = 0f
                for (k in 0 until 3) sum += a[k * 3 + row] * b[col * 3 + k]
                out[col * 3 + row] = sum
            }
        }
        return Affine(out)
    }

    companion object {
        fun of(a: Float, b: Float, tx: Float, c: Float, d: Float, ty: Float): Affine =
            Affine(floatArrayOf(a, c, 0f, b, d, 0f, tx, ty, 1f))

        val IDENTITY: Affine = of(1f, 0f, 0f, 0f, 1f, 0f)

        fun translate(x: Float, y: Float): Affine = of(1f, 0f, x, 0f, 1f, y)

        /** A turn by [degrees], a multiple of 90, as `android.graphics.Matrix.setRotate`
         *  builds it (`x' = cos x - sin y`, `y' = sin x + cos y`). */
        fun rotate(degrees: Int): Affine {
            val (cos, sin) = when (((degrees % 360) + 360) % 360) {
                0 -> 1f to 0f
                90 -> 0f to 1f
                180 -> -1f to 0f
                270 -> 0f to -1f
                else -> 1f to 0f
            }
            return of(cos, -sin, 0f, sin, cos, 0f)
        }

        /** A turn about the centre of the unit square. */
        fun rotateAboutCentre(degrees: Int): Affine =
            translate(0.5f, 0.5f).after(rotate(degrees)).after(translate(-0.5f, -0.5f))
    }
}

/**
 * Where the upright picture sits inside the model input, in model pixels:
 * the Rust core's rule (`backgroundModelInputRect`, shape kept, centred,
 * bars where the shapes differ), never computed here, so every client
 * segments the same picture.
 */
internal data class ModelFit(val offsetX: Float, val offsetY: Float, val width: Float, val height: Float)

internal object BackgroundGeometry {
    /** The upright size of a [width] x [height] buffer turned by [rotation]. */
    fun uprightSize(width: Int, height: Int, rotation: Int): Pair<Int, Int> =
        if (((rotation % 180) + 180) % 180 == 90) height to width else width to height

    /** Buffer point -> upright point. */
    fun bufferToUpright(rotation: Int): Affine = Affine.rotateAboutCentre(-rotation)

    /** Upright point -> buffer point (what `VideoFrameDrawer` samples). */
    fun uprightToBuffer(rotation: Int): Affine = Affine.rotateAboutCentre(rotation)

    /** Upright point -> model point (`y` 0 at the picture's top row). */
    fun uprightToModel(fit: ModelFit, modelW: Int, modelH: Int): Affine = Affine.of(
        fit.width / modelW, 0f, fit.offsetX / modelW,
        0f, -fit.height / modelH, (fit.offsetY + fit.height) / modelH,
    )

    /** Model point -> upright point. */
    fun modelToUpright(fit: ModelFit, modelW: Int, modelH: Int): Affine = Affine.of(
        modelW / fit.width, 0f, -fit.offsetX / fit.width,
        0f, -modelH / fit.height, (fit.offsetY + fit.height) / fit.height,
    )

    /** For the model pass: model output point -> buffer point to sample. */
    fun modelToBuffer(rotation: Int, fit: ModelFit, modelW: Int, modelH: Int): Affine =
        uprightToBuffer(rotation).after(modelToUpright(fit, modelW, modelH))

    /** For the blend: output (buffer) point -> mask point. */
    fun bufferToMask(rotation: Int, fit: ModelFit, modelW: Int, modelH: Int): Affine =
        uprightToModel(fit, modelW, modelH).after(bufferToUpright(rotation))

    /**
     * For the blend: output (buffer) point -> picture texture point, the
     * picture's rows uploaded top first, cropped by [cover] (x, y, width,
     * height in picture pixels, `y` from the top) of a [pictureW] x
     * [pictureH] picture.
     */
    fun bufferToPicture(
        rotation: Int,
        cover: FloatArray,
        pictureW: Int,
        pictureH: Int,
    ): Affine {
        val (cx, cy, cw, ch) = listOf(cover[0], cover[1], cover[2], cover[3])
        val uprightToPicture = Affine.of(
            cw / pictureW, 0f, cx / pictureW,
            0f, -ch / pictureH, (cy + ch) / pictureH,
        )
        return uprightToPicture.after(bufferToUpright(rotation))
    }
}
