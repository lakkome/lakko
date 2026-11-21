// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import android.graphics.BitmapFactory
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import me.lakko.app.chat.AttachmentBridge
import me.lakko.app.rethrowIfCancelled
import me.lakko.app.storage.BlobLifetime
import me.lakko.app.storage.StorageClient
import uniffi.lakko_ffi_uniffi.AttachmentRef

/**
 * The Android half of the call chat's pictures (`docs/ux/calls.md` section
 * 4.1.4.1): preparing a pasted picture as a conversation photo is prepared,
 * encrypting with `lakko-attachments`, the upload with an idle life of one
 * day, the download, and the owed delete at the end of the call. No log line
 * names a blob, a key or a peer.
 */

/** Section 4.1.4.1 (ADR-0087 section 13): a call-chat picture's idle life in
 *  the file store, in days. */
public const val CALL_CHAT_PICTURE_IDLE_DAYS: Int = 1

/** The core's largest call-chat picture, in bytes. */
internal fun callChatMaxPictureBytes(): Int =
    runCatching { uniffi.lakko_ffi_uniffi.callChatMaxPictureBytes().toInt() }
        .getOrDefault(CALL_CHAT_DEFAULT_MAX_PICTURE_BYTES)

/** The conversation's photo limit, the longest side in pixels. */
internal fun callChatPhotoMaxSide(): Int =
    runCatching { uniffi.lakko_ffi_uniffi.photoMaxLongestSidePx().toInt() }.getOrDefault(2560)

/** Prepare pasted [bytes]: decoded, the longest side brought down to the
 *  conversation's limit, re-encoded as JPEG with no metadata. Off the main
 *  thread. */
internal suspend fun prepareCallChatPaste(bytes: ByteArray): CallChatPaste = withContext(Dispatchers.Default) {
    prepareCallChatPicture(
        bytes = bytes,
        maxLongestSide = callChatPhotoMaxSide(),
        maxBytes = callChatMaxPictureBytes(),
        reencode = { input, max -> me.lakko.app.media.reencodePhoto(input, max) },
        boundsOf = ::jpegBounds,
    )
}

private fun jpegBounds(jpeg: ByteArray): CallChatPictureSize? = runCatching {
    val options = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeByteArray(jpeg, 0, jpeg.size, options)
    if (options.outWidth > 0 && options.outHeight > 0) CallChatPictureSize(options.outWidth, options.outHeight) else null
}.getOrNull()

/** The production work behind [CallChatSender]. */
internal fun callChatPictureDeps(applicationContext: Context): CallChatPictureDeps = CallChatPictureDeps(
    encrypt = { jpeg ->
        withContext(Dispatchers.Default) {
            try {
                val enc = uniffi.lakko_ffi_uniffi.encryptAttachment(plaintext = jpeg)
                EncryptedCallChatPicture(enc.blobId, enc.key, enc.nonce, enc.ciphertext)
            } catch (e: Throwable) {
                rethrowIfCancelled(e)
                android.util.Log.w(TAG, "picture did not encrypt: ${e.javaClass.simpleName}")
                null
            }
        }
    },
    put = { picture ->
        withContext(Dispatchers.IO) {
            try {
                StorageClient().use { client ->
                    client.putBlob(
                        blobId = picture.blobId,
                        ciphertext = picture.ciphertext,
                        idleDays = CALL_CHAT_PICTURE_IDLE_DAYS,
                    )
                }
                true
            } catch (e: Throwable) {
                rethrowIfCancelled(e)
                android.util.Log.w(TAG, "picture upload failed: ${e.javaClass.simpleName}")
                false
            }
        }
    },
    download = { picture ->
        withContext(Dispatchers.IO) {
            try {
                AttachmentBridge.downloadAndDecrypt(
                    AttachmentRef(
                        blobId = picture.blobId,
                        key = picture.key,
                        nonce = picture.nonce,
                        mime = "image/jpeg",
                        size = picture.size.toULong(),
                        name = null,
                    ),
                ).takeIf { it.isNotEmpty() && it.size <= callChatMaxPictureBytes() }
            } catch (e: Throwable) {
                rethrowIfCancelled(e)
                android.util.Log.w(TAG, "picture download failed: ${e.javaClass.simpleName}")
                null
            }
        }
    },
    deleteUploaded = { ids -> BlobLifetime.delete(applicationContext, ids) },
)

private const val TAG = "CallChatPicture"
