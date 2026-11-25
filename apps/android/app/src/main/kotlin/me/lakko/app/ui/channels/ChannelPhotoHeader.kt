// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import me.lakko.app.avatar.AvatarMedia
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.ui.components.LakkoAvatar
import me.lakko.app.ui.components.LakkoDialog
import me.lakko.app.ui.components.LakkoDialogAction
import me.lakko.app.ui.components.LakkoDialogTone
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize

/**
 * ADR-0087 section 18.2 / UX 4.5.2 — a channel's 96 dp header avatar, with
 * the profile photo's edit button and menu for the owner: `Photo or video`,
 * `Camera`, `Remove photo`. `Use` on the crop screen is the confirmation: the
 * still (and the video) are uploaded and the channel record is posted with
 * the new photo; `Remove photo` asks first.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ChannelPhotoHeader(
    title: String,
    seed: String,
    avatar: me.lakko.app.profile.AvatarRefs?,
    canSet: Boolean,
    onUse: suspend (AvatarMedia.Encoded) -> Unit,
    onRemove: suspend () -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var uploading by remember { mutableStateOf(false) }
    var failure by remember { mutableStateOf<UiText?>(null) }
    var menuOpen by remember { mutableStateOf(false) }
    var confirmRemove by remember { mutableStateOf(false) }
    var cropSource by remember { mutableStateOf<AvatarMedia.Source?>(null) }
    var cameraOpen by remember { mutableStateOf(false) }
    var pendingStill by remember { mutableStateOf<android.graphics.Bitmap?>(null) }
    val picker = rememberLauncherForActivityResult(ActivityResultContracts.PickVisualMedia()) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        scope.launch {
            val source = AvatarMedia.open(context, uri)
            if (source == null) failure = UiText("profile.video.unusable") else cropSource = source
        }
    }
    val hasCamera = remember { me.lakko.app.media.VideoNoteRecorder.hasAnyCamera(context) }
    val changeLabel = t("profile.avatar.change")

    fun failureOf(cause: Throwable): UiText {
        var c: Throwable? = cause
        val seen = HashSet<Throwable>()
        while (c != null && seen.add(c)) {
            if (c is me.lakko.app.storage.StorageHttpError && c.status == 429) return UiText("group_photo.error.limit")
            me.lakko.app.media.groupPhotoRefusalKey(c)?.let { return UiText(it) }
            c = c.cause
        }
        return UiText("group_photo.error.upload")
    }

    Column(horizontalAlignment = Alignment.CenterHorizontally, modifier = Modifier.fillMaxWidth()) {
        Box(contentAlignment = Alignment.Center) {
            LakkoAvatar(
                name = title,
                seed = seed,
                size = 96.dp,
                group = true,
                avatar = avatar,
                large = true,
                localStill = pendingStill,
                modifier = if (canSet && !uploading) Modifier.clickable { menuOpen = true } else Modifier,
            )
            if (uploading) CircularProgressIndicator(modifier = Modifier.size(104.dp))
            if (canSet) {
                Surface(
                    shape = CircleShape,
                    color = MaterialTheme.colorScheme.primary,
                    contentColor = MaterialTheme.colorScheme.onPrimary,
                    modifier = Modifier
                        .align(Alignment.BottomEnd)
                        .size(32.dp)
                        .semantics { contentDescription = changeLabel }
                        .clickable(enabled = !uploading) { menuOpen = true },
                ) {
                    Box(contentAlignment = Alignment.Center) {
                        LkIcon(R.drawable.ic_lk_pencil, contentDescription = null, size = LkIconSize.Row)
                    }
                }
            }
        }
        failure?.let {
            Text(it.text(), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(top = 6.dp))
        }
    }

    if (menuOpen) {
        me.lakko.app.ui.components.PhotoMenu(
            hasCamera = hasCamera,
            canRemove = avatar != null,
            onDismiss = { menuOpen = false },
            onLibrary = { picker.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageAndVideo)) },
            onCamera = { cameraOpen = true },
            onRemove = { confirmRemove = true },
        )
    }
    if (confirmRemove) {
        LakkoDialog(
            onDismissRequest = { confirmRemove = false },
            title = t("channels.photo.remove_title"),
            body = t("channels.photo.remove_body"),
            confirm = LakkoDialogAction(t("group_photo.remove.confirm"), LakkoDialogTone.Danger) {
                confirmRemove = false
                scope.launch {
                    uploading = true
                    failure = null
                    try {
                        onRemove()
                    } catch (e: kotlinx.coroutines.CancellationException) {
                        throw e
                    } catch (e: Throwable) {
                        failure = UiText("group_handle.error.failed")
                    } finally {
                        uploading = false
                    }
                }
            },
        )
    }
    cropSource?.let { source ->
        me.lakko.app.ui.profile.AvatarCropScreen(
            source = source,
            onCancel = { cropSource = null },
            onUse = { encoded ->
                cropSource = null
                pendingStill = encoded.stillBitmap
                scope.launch {
                    uploading = true
                    failure = null
                    try {
                        onUse(encoded)
                    } catch (e: kotlinx.coroutines.CancellationException) {
                        throw e
                    } catch (e: Throwable) {
                        failure = failureOf(e)
                    } finally {
                        uploading = false
                        pendingStill = null
                    }
                }
            },
        )
    }
    if (cameraOpen) {
        me.lakko.app.ui.profile.ProfileCameraScreen(
            onClose = { cameraOpen = false },
            onCaptured = { source ->
                cameraOpen = false
                cropSource = source
            },
        )
    }
}

/** Section 18.2 — upload a cropped photo as a channel photo: the still and
 *  the optional video, each an encrypted blob. */
internal suspend fun uploadChannelPhoto(context: android.content.Context, encoded: AvatarMedia.Encoded): me.lakko.app.profile.AvatarRefs {
    val still = me.lakko.app.chat.AttachmentBridge.encryptAndUpload(bytes = encoded.still, mime = "image/jpeg", name = null)
    val stillBlob = me.lakko.app.profile.avatarBlobOf(still.blobId, still.key, still.nonce, encoded.still.size.toLong(), encoded.stillSide)
    me.lakko.app.avatar.AvatarImages.putOwn(context, stillBlob, encoded.still)
    val videoFile = encoded.video
    val videoSide = encoded.videoSide
    val videoMs = encoded.videoDurationMs
    val videoBlob = if (videoFile != null && videoSide != null && videoMs != null) {
        val bytes = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { videoFile.readBytes() }
        val ref = me.lakko.app.chat.AttachmentBridge.encryptAndUpload(bytes = bytes, mime = "video/mp4", name = null)
        me.lakko.app.profile.avatarBlobOf(ref.blobId, ref.key, ref.nonce, bytes.size.toLong(), videoSide).also {
            me.lakko.app.avatar.AvatarImages.putOwn(context, it, bytes)
        }
    } else {
        null
    }
    return me.lakko.app.profile.AvatarRefs(
        still = stillBlob,
        video = videoBlob,
        videoDurationMs = if (videoBlob != null) videoMs else null,
    )
}
