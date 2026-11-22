// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.media.ExifInterface
import android.net.Uri
import java.io.File
import me.lakko.app.media.PickedRead
import me.lakko.app.media.mimeOfFileName
import me.lakko.app.media.readPickedUri
import me.lakko.app.media.reencodePhoto
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.GraphicsMode

/**
 * UX attachments section 5 — what the in-app camera takes reaches the
 * composer tray as a photo or a video, the same as one picked through Photo
 * or Video. Seen live on a Pixel: the camera handed the tray a `file://` URI,
 * which no content provider answers, so the take had no type and no name and
 * went out as an unnamed 3.7 MB file that was neither re-encoded nor turned
 * upright.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class CameraTakeTest {
    private lateinit var context: Context

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
        File(context.cacheDir, CAMERA_TAKE_DIR).deleteRecursively()
    }

    @After
    fun tearDown() {
        File(context.cacheDir, CAMERA_TAKE_DIR).deleteRecursively()
    }

    @Test
    fun aTakeIsWrittenAsAPhotoOrAVideoWithAPlainName() {
        val photo = cameraTakeFile(context.cacheDir, CameraTakeKind.PHOTO, "a")
        val video = cameraTakeFile(context.cacheDir, CameraTakeKind.VIDEO, "b")
        assertEquals("photo.jpg", photo.name)
        assertEquals("video.mp4", video.name)
        assertTrue(photo.parentFile!!.isDirectory)
        assertEquals("image/jpeg", cameraTakeMime(CameraTakeKind.PHOTO))
        assertEquals("video/mp4", cameraTakeMime(CameraTakeKind.VIDEO))
    }

    @Test
    fun theTrayTakesTheCameraMimeForAPhotoAndAVideoNotAFile() {
        assertEquals(TrayKind.PHOTO, trayKindOf(cameraTakeMime(CameraTakeKind.PHOTO), filesOnly = false))
        assertEquals(TrayKind.VIDEO, trayKindOf(cameraTakeMime(CameraTakeKind.VIDEO), filesOnly = false))
        // The defect: with no type the take was a file.
        assertEquals(TrayKind.FILE, trayKindOf(null, filesOnly = false))
    }

    @Test
    fun readingAFileUriTakesTheTypeAndNameFromTheFile() {
        val file = cameraTakeFile(context.cacheDir, CameraTakeKind.PHOTO, "c")
        file.writeBytes(jpeg(40, 20, ExifInterface.ORIENTATION_NORMAL))
        val read = readPickedUri(context, Uri.fromFile(file), maxBytes = 1_000_000)
        assertTrue("read failed", read is PickedRead.Ok)
        val payload = (read as PickedRead.Ok).payload
        assertEquals("image/jpeg", payload.mime)
        assertEquals("photo.jpg", payload.name)
        assertEquals(file.length().toInt(), payload.bytes.size)
    }

    @Test
    fun anOversizedFileUriIsRefusedBeforeItIsRead() {
        val file = cameraTakeFile(context.cacheDir, CameraTakeKind.VIDEO, "d")
        file.writeBytes(ByteArray(2_000))
        val read = readPickedUri(context, Uri.fromFile(file), maxBytes = 1_000)
        assertTrue(read is PickedRead.Failed)
        assertEquals("video.mp4", (read as PickedRead.Failed).name)
    }

    @Test
    fun anUprightCapturedPhotoIsReEncodedUpright() {
        // CameraX writes the sensor's landscape frame with an EXIF rotation.
        val captured = jpeg(40, 20, ExifInterface.ORIENTATION_ROTATE_90)
        val sent = reencodePhoto(captured, 2560)
        assertNotNull(sent)
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeByteArray(sent, 0, sent!!.size, bounds)
        assertEquals(20, bounds.outWidth)
        assertEquals(40, bounds.outHeight)
    }

    @Test
    fun theExtensionTableKnowsOnlyMediaTypes() {
        assertEquals("image/jpeg", mimeOfFileName("photo.JPG"))
        assertEquals("video/mp4", mimeOfFileName("video.mp4"))
        assertNull(mimeOfFileName("notes.txt"))
        assertNull(mimeOfFileName("noextension"))
    }

    @Test
    fun oldTakesAreRemovedAndRecentOnesKept() {
        val old = cameraTakeFile(context.cacheDir, CameraTakeKind.PHOTO, "old").parentFile!!
        val recent = cameraTakeFile(context.cacheDir, CameraTakeKind.PHOTO, "recent").parentFile!!
        val now = 10L * CAMERA_TAKE_KEEP_MS
        old.setLastModified(now - CAMERA_TAKE_KEEP_MS - 1_000)
        recent.setLastModified(now - 1_000)
        pruneCameraTakes(context.cacheDir, now)
        assertFalse(old.exists())
        assertTrue(recent.exists())
    }

    private fun jpeg(width: Int, height: Int, orientation: Int): ByteArray {
        val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
        val file = File.createTempFile("exif", ".jpg", context.cacheDir)
        try {
            file.outputStream().use { bitmap.compress(Bitmap.CompressFormat.JPEG, 90, it) }
            ExifInterface(file.absolutePath).apply {
                setAttribute(ExifInterface.TAG_ORIENTATION, orientation.toString())
                saveAttributes()
            }
            return file.readBytes()
        } finally {
            file.delete()
        }
    }
}
