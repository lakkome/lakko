// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.graphics.Bitmap
import android.opengl.GLES20
import android.opengl.GLUtils
import android.os.Handler
import android.os.Looper
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.FloatBuffer
import java.util.concurrent.Executor
import java.util.concurrent.atomic.AtomicBoolean
import org.webrtc.GlRectDrawer
import org.webrtc.GlShader
import org.webrtc.GlTextureFrameBuffer
import org.webrtc.TextureBufferImpl
import org.webrtc.VideoFrame
import org.webrtc.VideoFrameDrawer
import org.webrtc.YuvConverter

/**
 * The camera background's GPU half on Android (ADR-0089 section 2): OpenGL
 * ES 2 on the camera's capture thread, which is libwebrtc's
 * `SurfaceTextureHelper` thread and holds an EGL context in the call's
 * shared EGL group, so a texture made here is readable by the encoders and
 * the renderers.
 *
 * Per frame:
 *  1. the camera texture (OES, with its transform) is drawn into a plain
 *     RGBA texture of the frame's own size ([frame]);
 *  2. when a model run is due, the frame is drawn upright into the 256 x
 *     144 model input and read back ([prepareModelInput]);
 *  3. for a blur, the frame is scaled down by the core's divisor and blurred
 *     in two separable Gaussian passes with the core's sigma and radius;
 *     for a picture, the picture is uploaded once;
 *  4. the frame is blended over the blur or the picture with the latest
 *     mask (`out = frame * alpha + background * (1 - alpha)`), into an
 *     output texture of the frame's size, handed on as a new texture frame
 *     with the camera frame's rotation and timestamp.
 *
 * The geometry is [BackgroundGeometry]. Everything here runs on the capture
 * thread, except picture decoding, which runs on [pictureWorker].
 */
internal class BackgroundGlCompositor(
    private val modelWidth: Int,
    private val modelHeight: Int,
    private val pictureWorker: Executor,
    private val pictures: BackgroundPictureSource,
) : BackgroundCompositor {

    private var drawer: GlRectDrawer? = null
    private var yuvDrawer: VideoFrameDrawer? = null
    private var frameFb: GlTextureFrameBuffer? = null
    private var modelFb: GlTextureFrameBuffer? = null
    private var smallFb: GlTextureFrameBuffer? = null
    private var blurFbA: GlTextureFrameBuffer? = null
    private var blurFbB: GlTextureFrameBuffer? = null
    private var mapShader: GlShader? = null
    private var blurShader: GlShader? = null
    private var blendShader: GlShader? = null
    private var maskTexture = 0
    private var pictureTexture = 0
    private var pictureKey: String? = null
    private var pictureSize = 0 to 0
    private var yuvConverter: YuvConverter? = null
    private var handler: Handler? = null
    private val outputs = ArrayList<Output>()
    private val quad: FloatBuffer = floatBuffer(floatArrayOf(-1f, -1f, 1f, -1f, -1f, 1f, 1f, 1f))

    /** A picture being loaded or loaded, for [pictureKey]. */
    @Volatile private var pending: PendingPicture? = null

    private class PendingPicture(val key: String) {
        @Volatile var done = false
        @Volatile var picture: BackgroundPicture? = null
    }

    private class Output(val fb: GlTextureFrameBuffer) {
        val free = AtomicBoolean(true)
    }

    // ---- 1 and 2 -----------------------------------------------------------

    override fun prepareModelInput(frame: VideoFrame, out: ByteBuffer): Boolean {
        val src = unpack(frame) ?: return false
        val upright = BackgroundGeometry.uprightSize(frame.buffer.width, frame.buffer.height, frame.rotation)
        val fit = modelFit(upright)
        val map = BackgroundGeometry.modelToBuffer(frame.rotation, fit, modelWidth, modelHeight)
        val fb = modelFb ?: GlTextureFrameBuffer(GLES20.GL_RGBA).also { modelFb = it }
        fb.setSize(modelWidth, modelHeight)
        drawMapped(src, map, fb, letterbox = true)
        out.clear()
        out.order(ByteOrder.nativeOrder())
        GLES20.glBindFramebuffer(GLES20.GL_FRAMEBUFFER, fb.frameBufferId)
        GLES20.glPixelStorei(GLES20.GL_PACK_ALIGNMENT, 1)
        GLES20.glReadPixels(0, 0, modelWidth, modelHeight, GLES20.GL_RGBA, GLES20.GL_UNSIGNED_BYTE, out)
        GLES20.glBindFramebuffer(GLES20.GL_FRAMEBUFFER, 0)
        checkGl("model input")
        out.rewind()
        return true
    }

    override fun uploadMask(mask: ByteBuffer) {
        if (maskTexture == 0) maskTexture = newTexture()
        GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, maskTexture)
        GLES20.glPixelStorei(GLES20.GL_UNPACK_ALIGNMENT, 1)
        mask.rewind()
        GLES20.glTexImage2D(
            GLES20.GL_TEXTURE_2D, 0, GLES20.GL_LUMINANCE, modelWidth, modelHeight, 0,
            GLES20.GL_LUMINANCE, GLES20.GL_UNSIGNED_BYTE, mask,
        )
        GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, 0)
        checkGl("mask upload")
    }

    // ---- 3 and 4 -----------------------------------------------------------

    override fun composite(frame: VideoFrame, look: BackgroundLook): CompositeResult {
        if (maskTexture == 0) return CompositeResult.NotReady
        val width = frame.buffer.width
        val height = frame.buffer.height
        val rotation = frame.rotation
        // The frame was unpacked for the model this frame, or is unpacked now.
        val src = if (unpackedFor === frame) frameFb else unpack(frame)
        src ?: return CompositeResult.NotReady
        val upright = BackgroundGeometry.uprightSize(width, height, rotation)
        val fit = modelFit(upright)
        val toMask = BackgroundGeometry.bufferToMask(rotation, fit, modelWidth, modelHeight)

        val background: Int
        val toBackground: Affine
        when (look) {
            is BackgroundLook.Blur -> {
                background = blur(src, width, height, look.strong)
                toBackground = Affine.IDENTITY
            }
            is BackgroundLook.Builtin, is BackgroundLook.Photo -> {
                when (val ready = picture(look, upright)) {
                    PictureState.Loading -> return CompositeResult.NotReady
                    PictureState.Unusable -> {
                        background = blur(src, width, height, strong = false)
                        toBackground = Affine.IDENTITY
                    }
                    is PictureState.Ready -> {
                        background = pictureTexture
                        toBackground = BackgroundGeometry.bufferToPicture(
                            rotation, ready.cover, pictureSize.first, pictureSize.second,
                        )
                    }
                }
            }
        }

        val output = acquireOutput(width, height) ?: return CompositeResult.NotReady
        val shader = blendShader ?: GlShader(VERTEX, BLEND_FRAGMENT).also { blendShader = it }
        GLES20.glBindFramebuffer(GLES20.GL_FRAMEBUFFER, output.fb.frameBufferId)
        GLES20.glViewport(0, 0, width, height)
        shader.useProgram()
        bindTexture(0, src.textureId)
        bindTexture(1, background)
        bindTexture(2, maskTexture)
        GLES20.glUniform1i(shader.getUniformLocation("u_frame"), 0)
        GLES20.glUniform1i(shader.getUniformLocation("u_bg"), 1)
        GLES20.glUniform1i(shader.getUniformLocation("u_mask"), 2)
        GLES20.glUniformMatrix3fv(shader.getUniformLocation("u_to_mask"), 1, false, toMask.m, 0)
        GLES20.glUniformMatrix3fv(shader.getUniformLocation("u_to_bg"), 1, false, toBackground.m, 0)
        drawQuad(shader)
        unbindTextures(3)
        GLES20.glBindFramebuffer(GLES20.GL_FRAMEBUFFER, 0)
        // The encoders and renderers read this texture from other EGL
        // contexts of the share group; it must be complete before they do.
        GLES20.glFinish()
        checkGl("blend")

        output.free.set(false)
        val handler = handler ?: Handler(Looper.myLooper() ?: Looper.getMainLooper()).also { this.handler = it }
        val converter = yuvConverter ?: YuvConverter().also { yuvConverter = it }
        val buffer = TextureBufferImpl(
            width,
            height,
            VideoFrame.TextureBuffer.Type.RGB,
            output.fb.textureId,
            android.graphics.Matrix(),
            handler,
            converter,
            Runnable { output.free.set(true) },
        )
        return CompositeResult.Frame(VideoFrame(buffer, rotation, frame.timestampNs))
    }

    override fun release() {
        drawer?.release()
        drawer = null
        yuvDrawer?.release()
        yuvDrawer = null
        listOfNotNull(frameFb, modelFb, smallFb, blurFbA, blurFbB).forEach { it.release() }
        frameFb = null
        modelFb = null
        smallFb = null
        blurFbA = null
        blurFbB = null
        outputs.forEach { it.fb.release() }
        outputs.clear()
        listOfNotNull(mapShader, blurShader, blendShader).forEach { it.release() }
        mapShader = null
        blurShader = null
        blendShader = null
        val textures = listOf(maskTexture, pictureTexture).filter { it != 0 }.toIntArray()
        if (textures.isNotEmpty()) GLES20.glDeleteTextures(textures.size, textures, 0)
        maskTexture = 0
        pictureTexture = 0
        pictureKey = null
        yuvConverter?.release()
        yuvConverter = null
        unpackedFor = null
    }

    // ---- passes ----------------------------------------------------------------

    /** The frame most recently drawn into [frameFb]. */
    private var unpackedFor: VideoFrame? = null

    /** Step 1: the camera frame as a plain RGBA texture of its own size, in
     *  buffer space; `null` for a buffer this stage does not draw. */
    private fun unpack(frame: VideoFrame): GlTextureFrameBuffer? {
        val width = frame.buffer.width
        val height = frame.buffer.height
        if (width <= 0 || height <= 0) return null
        val fb = frameFb ?: GlTextureFrameBuffer(GLES20.GL_RGBA).also { frameFb = it }
        fb.setSize(width, height)
        GLES20.glBindFramebuffer(GLES20.GL_FRAMEBUFFER, fb.frameBufferId)
        val buffer = frame.buffer
        val rectDrawer = drawer ?: GlRectDrawer().also { drawer = it }
        if (buffer is VideoFrame.TextureBuffer) {
            VideoFrameDrawer.drawTexture(rectDrawer, buffer, android.graphics.Matrix(), width, height, 0, 0, width, height)
        } else {
            // An I420 frame: drawn with the rotation taken back out, so it
            // lands in buffer space like a texture frame.
            val undo = android.graphics.Matrix().apply {
                preTranslate(0.5f, 0.5f)
                preRotate(-frame.rotation.toFloat())
                preTranslate(-0.5f, -0.5f)
            }
            val yuv = yuvDrawer ?: VideoFrameDrawer().also { yuvDrawer = it }
            yuv.drawFrame(frame, rectDrawer, undo, 0, 0, width, height)
        }
        GLES20.glBindFramebuffer(GLES20.GL_FRAMEBUFFER, 0)
        checkGl("unpack")
        unpackedFor = frame
        return fb
    }

    /** Draws [src] into [target] sampling it at [map] of each target point. */
    private fun drawMapped(src: GlTextureFrameBuffer, map: Affine, target: GlTextureFrameBuffer, letterbox: Boolean) {
        val shader = mapShader ?: GlShader(VERTEX, MAP_FRAGMENT).also { mapShader = it }
        GLES20.glBindFramebuffer(GLES20.GL_FRAMEBUFFER, target.frameBufferId)
        GLES20.glViewport(0, 0, target.width, target.height)
        shader.useProgram()
        bindTexture(0, src.textureId)
        GLES20.glUniform1i(shader.getUniformLocation("u_tex"), 0)
        GLES20.glUniformMatrix3fv(shader.getUniformLocation("u_map"), 1, false, map.m, 0)
        GLES20.glUniform1f(shader.getUniformLocation("u_letterbox"), if (letterbox) 1f else 0f)
        drawQuad(shader)
        unbindTextures(1)
        GLES20.glBindFramebuffer(GLES20.GL_FRAMEBUFFER, 0)
    }

    /** Step 3 for a blur: the frame scaled down and blurred; its texture,
     *  in buffer space. */
    private fun blur(src: GlTextureFrameBuffer, width: Int, height: Int, strong: Boolean): Int {
        val params = blurParameters(strong, minOf(width, height))
        val divisor = params.divisor.coerceAtLeast(1)
        val w = (width / divisor).coerceAtLeast(1)
        val h = (height / divisor).coerceAtLeast(1)
        val small = smallFb ?: GlTextureFrameBuffer(GLES20.GL_RGBA).also { smallFb = it }
        small.setSize(w, h)
        drawMapped(src, Affine.IDENTITY, small, letterbox = false)
        val a = blurFbA ?: GlTextureFrameBuffer(GLES20.GL_RGBA).also { blurFbA = it }
        val b = blurFbB ?: GlTextureFrameBuffer(GLES20.GL_RGBA).also { blurFbB = it }
        a.setSize(w, h)
        b.setSize(w, h)
        blurPass(small.textureId, a, 1f / w, 0f, params.sigma, params.radius)
        blurPass(a.textureId, b, 0f, 1f / h, params.sigma, params.radius)
        return b.textureId
    }

    private fun blurPass(source: Int, target: GlTextureFrameBuffer, dx: Float, dy: Float, sigma: Float, radius: Int) {
        val shader = blurShader ?: GlShader(VERTEX, BLUR_FRAGMENT).also { blurShader = it }
        GLES20.glBindFramebuffer(GLES20.GL_FRAMEBUFFER, target.frameBufferId)
        GLES20.glViewport(0, 0, target.width, target.height)
        shader.useProgram()
        bindTexture(0, source)
        GLES20.glUniform1i(shader.getUniformLocation("u_tex"), 0)
        GLES20.glUniform2f(shader.getUniformLocation("u_step"), dx, dy)
        GLES20.glUniform1f(shader.getUniformLocation("u_sigma"), sigma.coerceAtLeast(0.5f))
        GLES20.glUniform1f(shader.getUniformLocation("u_radius"), radius.coerceIn(1, MAX_RADIUS).toFloat())
        drawQuad(shader)
        unbindTextures(1)
        GLES20.glBindFramebuffer(GLES20.GL_FRAMEBUFFER, 0)
        checkGl("blur")
    }

    private var fitCache: Pair<Pair<Int, Int>, ModelFit>? = null

    /** The core's model-input rectangle for this upright size, asked once
     *  per size. */
    private fun modelFit(upright: Pair<Int, Int>): ModelFit {
        fitCache?.let { (size, fit) -> if (size == upright) return fit }
        val r = pictures.modelInputRect(upright.first, upright.second)
        val fit = ModelFit(r[0], r[1], r[2], r[3])
        fitCache = upright to fit
        return fit
    }

    private var blurCache: Triple<Boolean, Int, BlurParameters>? = null

    private fun blurParameters(strong: Boolean, frameHeight: Int): BlurParameters {
        blurCache?.let { (s, h, p) -> if (s == strong && h == frameHeight) return p }
        val p = pictures.blur(strong, frameHeight)
        blurCache = Triple(strong, frameHeight, p)
        return p
    }

    private sealed class PictureState {
        object Loading : PictureState()
        object Unusable : PictureState()
        class Ready(val cover: FloatArray) : PictureState()
    }

    private var coverCache: Pair<String, FloatArray>? = null

    /** Step 3 for a picture: uploaded once per picture and orientation. */
    private fun picture(look: BackgroundLook, upright: Pair<Int, Int>): PictureState {
        val key = when (look) {
            // A built-in is drawn for the frame's shape, so a phone turned
            // on its side gets a new drawing rather than a crop of the old.
            is BackgroundLook.Builtin -> "b:${look.id}:${builtinSize(upright)}"
            is BackgroundLook.Photo -> "p:${look.id}"
            is BackgroundLook.Blur -> return PictureState.Unusable
        }
        if (pictureKey != key) {
            val loading = pending
            if (loading == null || loading.key != key) {
                val fresh = PendingPicture(key)
                pending = fresh
                pictureWorker.execute {
                    fresh.picture = try {
                        when (look) {
                            is BackgroundLook.Builtin -> {
                                val (w, h) = builtinSize(upright)
                                pictures.builtin(look.id, w, h)
                            }
                            is BackgroundLook.Photo -> pictures.photo(look.id)
                            is BackgroundLook.Blur -> null
                        }
                    } catch (_: Throwable) {
                        null
                    }
                    fresh.done = true
                }
                return PictureState.Loading
            }
            if (!loading.done) return PictureState.Loading
            val loaded = loading.picture ?: return PictureState.Unusable
            uploadPicture(loaded)
            pictureKey = key
            coverCache = null
        }
        val coverKey = "$key@${upright.first}x${upright.second}"
        val cover = coverCache?.takeIf { it.first == coverKey }?.second
            ?: pictures.cover(pictureSize.first, pictureSize.second, upright.first, upright.second)
                .also { coverCache = coverKey to it }
        return PictureState.Ready(cover)
    }

    private fun uploadPicture(picture: BackgroundPicture) {
        if (pictureTexture == 0) pictureTexture = newTexture()
        GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, pictureTexture)
        GLES20.glPixelStorei(GLES20.GL_UNPACK_ALIGNMENT, 1)
        when (picture) {
            is BackgroundPicture.Rgba -> {
                val buffer = ByteBuffer.allocateDirect(picture.pixels.size).put(picture.pixels)
                buffer.rewind()
                GLES20.glTexImage2D(
                    GLES20.GL_TEXTURE_2D, 0, GLES20.GL_RGBA, picture.width, picture.height, 0,
                    GLES20.GL_RGBA, GLES20.GL_UNSIGNED_BYTE, buffer,
                )
                pictureSize = picture.width to picture.height
            }
            is BackgroundPicture.Image -> {
                GLUtils.texImage2D(GLES20.GL_TEXTURE_2D, 0, picture.bitmap, 0)
                pictureSize = picture.bitmap.width to picture.bitmap.height
                picture.bitmap.recycle()
            }
        }
        GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, 0)
        checkGl("picture upload")
    }

    private fun builtinSize(upright: Pair<Int, Int>): Pair<Int, Int> {
        val (w, h) = upright
        val longer = maxOf(w, h).coerceAtLeast(1)
        val scale = minOf(1f, BUILTIN_MAX_SIDE.toFloat() / longer)
        return (w * scale).toInt().coerceAtLeast(1) to (h * scale).toInt().coerceAtLeast(1)
    }

    private fun acquireOutput(width: Int, height: Int): Output? {
        for (output in outputs) {
            if (output.free.get()) {
                if (output.fb.width != width || output.fb.height != height) output.fb.setSize(width, height)
                return output
            }
        }
        if (outputs.size >= MAX_OUTPUTS) return null
        val fb = GlTextureFrameBuffer(GLES20.GL_RGBA)
        fb.setSize(width, height)
        val output = Output(fb)
        outputs += output
        return output
    }

    // ---- GL helpers ------------------------------------------------------------

    private fun drawQuad(shader: GlShader) {
        shader.setVertexAttribArray("in_pos", 2, quad)
        GLES20.glDrawArrays(GLES20.GL_TRIANGLE_STRIP, 0, 4)
    }

    private fun bindTexture(unit: Int, texture: Int) {
        GLES20.glActiveTexture(GLES20.GL_TEXTURE0 + unit)
        GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, texture)
    }

    private fun unbindTextures(count: Int) {
        for (unit in count - 1 downTo 0) {
            GLES20.glActiveTexture(GLES20.GL_TEXTURE0 + unit)
            GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, 0)
        }
    }

    private fun newTexture(): Int {
        val ids = IntArray(1)
        GLES20.glGenTextures(1, ids, 0)
        GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, ids[0])
        GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_MIN_FILTER, GLES20.GL_LINEAR)
        GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_MAG_FILTER, GLES20.GL_LINEAR)
        GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_WRAP_S, GLES20.GL_CLAMP_TO_EDGE)
        GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_WRAP_T, GLES20.GL_CLAMP_TO_EDGE)
        GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, 0)
        return ids[0]
    }

    private fun checkGl(what: String) {
        val error = GLES20.glGetError()
        if (error != GLES20.GL_NO_ERROR) throw IllegalStateException("GL error $error in $what")
    }

    private companion object {
        /** Output textures kept in flight at once (frames the encoders and
         *  renderers still hold). Chosen, not measured. */
        const val MAX_OUTPUTS = 8

        /** The longest side a built-in picture is drawn at. */
        const val BUILTIN_MAX_SIDE = 1280

        /** The most taps on each side the blur shader runs. The core's
         *  radius stays under 37 for any frame up to 4096 pixels. */
        const val MAX_RADIUS = 40

        fun floatBuffer(values: FloatArray): FloatBuffer =
            ByteBuffer.allocateDirect(values.size * 4).order(ByteOrder.nativeOrder()).asFloatBuffer().apply {
                put(values)
                rewind()
            }

        const val VERTEX = """
attribute vec2 in_pos;
varying vec2 v_uv;
void main() {
  v_uv = in_pos * 0.5 + 0.5;
  gl_Position = vec4(in_pos, 0.0, 1.0);
}
"""

        /** Samples [u_tex] at `u_map * point`; with `u_letterbox` set, a
         *  point that falls outside the picture is black (the model's bars). */
        const val MAP_FRAGMENT = """
precision mediump float;
varying vec2 v_uv;
uniform sampler2D u_tex;
uniform mat3 u_map;
uniform float u_letterbox;
void main() {
  vec2 uv = (u_map * vec3(v_uv, 1.0)).xy;
  bool outside = uv.x < 0.0 || uv.y < 0.0 || uv.x > 1.0 || uv.y > 1.0;
  if (u_letterbox > 0.5 && outside) {
    gl_FragColor = vec4(0.0, 0.0, 0.0, 1.0);
  } else {
    gl_FragColor = vec4(texture2D(u_tex, uv).rgb, 1.0);
  }
}
"""

        /** One direction of a Gaussian: taps from `-u_radius` to
         *  `u_radius`, weights `exp(-i^2 / 2 sigma^2)`, normalised. */
        const val BLUR_FRAGMENT = """
precision mediump float;
varying vec2 v_uv;
uniform sampler2D u_tex;
uniform vec2 u_step;
uniform float u_sigma;
uniform float u_radius;
void main() {
  vec3 sum = vec3(0.0);
  float total = 0.0;
  float twoSigma2 = 2.0 * u_sigma * u_sigma;
  for (int i = -40; i <= 40; i++) {
    float f = float(i);
    if (abs(f) > u_radius) continue;
    float w = exp(-(f * f) / twoSigma2);
    sum += texture2D(u_tex, v_uv + f * u_step).rgb * w;
    total += w;
  }
  gl_FragColor = vec4(sum / total, 1.0);
}
"""

        /** `out = frame * alpha + background * (1 - alpha)`, alpha the mask
         *  (255 = the person) sampled bilinearly. */
        const val BLEND_FRAGMENT = """
precision mediump float;
varying vec2 v_uv;
uniform sampler2D u_frame;
uniform sampler2D u_bg;
uniform sampler2D u_mask;
uniform mat3 u_to_mask;
uniform mat3 u_to_bg;
void main() {
  vec3 frame = texture2D(u_frame, v_uv).rgb;
  float alpha = texture2D(u_mask, (u_to_mask * vec3(v_uv, 1.0)).xy).r;
  vec3 bg = texture2D(u_bg, (u_to_bg * vec3(v_uv, 1.0)).xy).rgb;
  gl_FragColor = vec4(frame * alpha + bg * (1.0 - alpha), 1.0);
}
"""
    }
}

/** A picture to draw behind the person. */
internal sealed class BackgroundPicture {
    /** RGBA rows, top first (a built-in, drawn by the core). */
    class Rgba(val width: Int, val height: Int, val pixels: ByteArray) : BackgroundPicture()

    /** A decoded own photo. */
    class Image(val bitmap: Bitmap) : BackgroundPicture()
}

/** The core's blur parameters (`backgroundBlur`). */
internal data class BlurParameters(val divisor: Int, val sigma: Float, val radius: Int)

/** Where the compositor's pictures and numbers come from. */
internal interface BackgroundPictureSource {
    /** Built-in [id] drawn at [width] x [height]; `null` when it cannot be. */
    fun builtin(id: String, width: Int, height: Int): BackgroundPicture?

    /** Own photo [id] decoded; `null` when missing or unreadable. */
    fun photo(id: String): BackgroundPicture?

    /** The blur for a frame whose short side is [frameHeight]. */
    fun blur(strong: Boolean, frameHeight: Int): BlurParameters

    /** Where an upright frame of this size sits in the model input: x, y,
     *  width, height in model pixels (`backgroundModelInputRect`). */
    fun modelInputRect(frameWidth: Int, frameHeight: Int): FloatArray

    /** The cover rectangle: x, y, width, height in source pixels. */
    fun cover(srcWidth: Int, srcHeight: Int, dstWidth: Int, dstHeight: Int): FloatArray
}
