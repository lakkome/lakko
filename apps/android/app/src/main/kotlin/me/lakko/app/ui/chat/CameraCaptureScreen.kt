// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageCapture
import androidx.camera.core.ImageCaptureException
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.video.FileOutputOptions
import androidx.camera.video.Quality
import androidx.camera.video.QualitySelector
import androidx.camera.video.Recorder
import androidx.camera.video.Recording
import androidx.camera.video.VideoCapture
import androidx.camera.video.VideoRecordEvent
import androidx.camera.view.PreviewView
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.core.content.ContextCompat
import me.lakko.app.l10n.t
import java.io.File

/** UX attachments section 5 — the longest video the camera records. */
internal const val CAMERA_VIDEO_MAX_MS: Long = 60_000L

/** What the camera took: a JPEG photo or an MP4 video. */
internal enum class CameraTakeKind { PHOTO, VIDEO }

/**
 * One take, handed to the composer tray. [mime] travels as the tray's MIME
 * hint: a `file://` URI has no content provider to answer `getType`, and
 * without a type the tray took the take for a generic file (no re-encode,
 * no orientation, no thumbnail, no name).
 */
internal data class CameraTake(val uri: Uri, val mime: String)

/** The directory under the app cache that holds the camera's takes. */
internal const val CAMERA_TAKE_DIR: String = "camera"

/** How long a take's file is kept before the next camera open removes it. */
internal const val CAMERA_TAKE_KEEP_MS: Long = 60L * 60L * 1000L

/** The MIME type a take of [kind] is written as. */
internal fun cameraTakeMime(kind: CameraTakeKind): String = when (kind) {
    CameraTakeKind.PHOTO -> "image/jpeg"
    CameraTakeKind.VIDEO -> "video/mp4"
}

/**
 * A new file for a take of [kind]: its own directory under [cacheDir], so the
 * file name the tray reads (and a video is sent under) is a plain
 * `photo.jpg` / `video.mp4` with no device timing in it.
 */
internal fun cameraTakeFile(cacheDir: File, kind: CameraTakeKind, unique: String): File {
    val dir = File(File(cacheDir, CAMERA_TAKE_DIR), unique)
    dir.mkdirs()
    return File(dir, if (kind == CameraTakeKind.PHOTO) "photo.jpg" else "video.mp4")
}

/**
 * Remove takes older than [CAMERA_TAKE_KEEP_MS]. The tray reads a take's bytes
 * as soon as it is added, so an hour-old take is no longer needed by anyone.
 */
internal fun pruneCameraTakes(cacheDir: File, nowMs: Long) {
    val root = File(cacheDir, CAMERA_TAKE_DIR)
    root.listFiles()?.forEach { dir ->
        if (nowMs - dir.lastModified() > CAMERA_TAKE_KEEP_MS) runCatching { dir.deleteRecursively() }
    }
}

/**
 * UX attachments section 5 (amended 2026-11-19) — Lakko's own capture view
 * (CameraX): the shutter (tap = photo; press and hold = video while held, or
 * the Photo / Video switch for a tap-only person), switch camera, flash,
 * close. After capture the take goes straight into the composer tray (media
 * messages section 11) and the view closes: no send preview, no Retake. The
 * tray treats it like a photo or video from Photo or Video (a photo is
 * re-encoded as JPEG with its orientation applied and its metadata dropped,
 * and gets its dimensions and thumbnails). Only the permission prompt is the
 * system's.
 */
@Composable
internal fun CameraCaptureScreen(onCaptured: (CameraTake) -> Unit, onClose: () -> Unit) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    var permitted by remember {
        mutableStateOf(ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED)
    }
    var denied by remember { mutableStateOf(false) }
    val askCamera = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { ok ->
        permitted = ok
        denied = !ok
    }
    val askMic = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { }
    LaunchedEffect(Unit) {
        runCatching { pruneCameraTakes(context.cacheDir, System.currentTimeMillis()) }
        if (!permitted) askCamera.launch(Manifest.permission.CAMERA)
    }
    var videoMode by remember { mutableStateOf(false) }
    var back by remember { mutableStateOf(true) }
    var flash by remember { mutableStateOf(false) }
    var recording by remember { mutableStateOf<Recording?>(null) }
    // A photo is being written: the shutter waits so one tap is one take.
    var saving by remember { mutableStateOf(false) }
    var done by remember { mutableStateOf(false) }
    val previewView = remember { PreviewView(context) }
    val imageCapture = remember { ImageCapture.Builder().build() }
    val videoCapture = remember {
        VideoCapture.withOutput(Recorder.Builder().setQualitySelector(QualitySelector.from(Quality.HD)).build())
    }
    fun deliver(file: File, kind: CameraTakeKind) {
        if (done) {
            file.parentFile?.deleteRecursively()
            return
        }
        done = true
        onCaptured(CameraTake(Uri.fromFile(file), cameraTakeMime(kind)))
        onClose()
    }
    if (permitted) {
        DisposableEffect(back, lifecycleOwner) {
            val future = ProcessCameraProvider.getInstance(context)
            var provider: ProcessCameraProvider? = null
            val preview = Preview.Builder().build().also { it.setSurfaceProvider(previewView.surfaceProvider) }
            future.addListener({
                provider = runCatching { future.get() }.getOrNull()
                runCatching {
                    provider?.unbindAll()
                    provider?.bindToLifecycle(
                        lifecycleOwner,
                        if (back) CameraSelector.DEFAULT_BACK_CAMERA else CameraSelector.DEFAULT_FRONT_CAMERA,
                        preview,
                        imageCapture,
                        videoCapture,
                    )
                }
            }, ContextCompat.getMainExecutor(context))
            onDispose { runCatching { provider?.unbind(preview, imageCapture, videoCapture) } }
        }
    }
    LaunchedEffect(flash) { imageCapture.flashMode = if (flash) ImageCapture.FLASH_MODE_ON else ImageCapture.FLASH_MODE_OFF }
    fun takePhoto() {
        if (saving || done) return
        val file = cameraTakeFile(context.cacheDir, CameraTakeKind.PHOTO, System.nanoTime().toString())
        // The rotation the person holds the phone at; CameraX writes it as the
        // JPEG's EXIF orientation and the tray applies it when it re-encodes.
        previewView.display?.rotation?.let { imageCapture.targetRotation = it }
        saving = true
        imageCapture.takePicture(
            ImageCapture.OutputFileOptions.Builder(file).build(),
            ContextCompat.getMainExecutor(context),
            object : ImageCapture.OnImageSavedCallback {
                override fun onImageSaved(output: ImageCapture.OutputFileResults) {
                    saving = false
                    deliver(file, CameraTakeKind.PHOTO)
                }
                override fun onError(exception: ImageCaptureException) {
                    saving = false
                    file.parentFile?.deleteRecursively()
                }
            },
        )
    }
    fun startVideo() {
        if (recording != null || saving || done) return
        val file = cameraTakeFile(context.cacheDir, CameraTakeKind.VIDEO, System.nanoTime().toString())
        val withAudio = ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) ==
            PackageManager.PERMISSION_GRANTED
        if (!withAudio) askMic.launch(Manifest.permission.RECORD_AUDIO)
        recording = runCatching {
            videoCapture.output.prepareRecording(context, FileOutputOptions.Builder(file).setDurationLimitMillis(CAMERA_VIDEO_MAX_MS).build())
                .let { if (withAudio) it.withAudioEnabled() else it }
                .start(ContextCompat.getMainExecutor(context)) { event ->
                    if (event is VideoRecordEvent.Finalize) {
                        recording = null
                        if (!event.hasError() || event.error == VideoRecordEvent.Finalize.ERROR_DURATION_LIMIT_REACHED) {
                            deliver(file, CameraTakeKind.VIDEO)
                        } else {
                            file.parentFile?.deleteRecursively()
                        }
                    }
                }
        }.getOrNull()
    }
    fun stopVideo() {
        recording?.stop()
    }
    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false)) {
        // Full screen and edge to edge, so the safe-drawing padding below is
        // what keeps the controls off the status and gesture bars.
        me.lakko.app.ui.call.EdgeToEdgeDialogWindow()
        Surface(color = Color.Black, modifier = Modifier.fillMaxSize()) {
            Box(Modifier.fillMaxSize()) {
                if (permitted && !denied) {
                    AndroidView(factory = { previewView }, modifier = Modifier.fillMaxSize())
                }
                Box(Modifier.fillMaxSize().safeDrawingPadding()) {
                    if (denied || !permitted) {
                        IconButton(onClick = onClose, modifier = Modifier.align(Alignment.TopStart).padding(8.dp)) {
                            me.lakko.app.ui.components.LkIcon(me.lakko.app.R.drawable.ic_lk_xmark, t("common.close"), tint = Color.White)
                        }
                        if (denied) {
                            Column(
                                Modifier.align(Alignment.Center).padding(24.dp),
                                verticalArrangement = Arrangement.spacedBy(12.dp),
                                horizontalAlignment = Alignment.CenterHorizontally,
                            ) {
                                Text(t("camera.denied"), color = Color.White)
                                OutlinedButton(onClick = {
                                    context.startActivity(
                                        Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", context.packageName, null))
                                            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                                    )
                                }) { Text(t("chat.record.open_settings")) }
                            }
                        }
                    } else {
                        Row(Modifier.fillMaxWidth().align(Alignment.TopCenter).padding(8.dp)) {
                            IconButton(onClick = onClose) { me.lakko.app.ui.components.LkIcon(me.lakko.app.R.drawable.ic_lk_xmark, t("common.close"), tint = Color.White) }
                            Box(Modifier.weight(1f))
                            IconButton(onClick = { flash = !flash }) {
                                me.lakko.app.ui.components.LkIcon(if (flash) me.lakko.app.R.drawable.ic_lk_bolt else me.lakko.app.R.drawable.ic_lk_bolt_slash, null, tint = Color.White)
                            }
                            IconButton(onClick = { back = !back }, enabled = recording == null && !saving) {
                                me.lakko.app.ui.components.LkIcon(me.lakko.app.R.drawable.ic_lk_camera_flip, t("camera.switch"), tint = Color.White)
                            }
                        }
                        Column(
                            Modifier.align(Alignment.BottomCenter).padding(bottom = 24.dp),
                            horizontalAlignment = Alignment.CenterHorizontally,
                            verticalArrangement = Arrangement.spacedBy(12.dp),
                        ) {
                            Row(horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                                TextButton(onClick = { videoMode = false }, enabled = recording == null) {
                                    Text(t("camera.photo"), color = if (!videoMode) MaterialTheme.colorScheme.primary else Color.White)
                                }
                                TextButton(onClick = { videoMode = true }, enabled = recording == null) {
                                    Text(t("camera.video"), color = if (videoMode) MaterialTheme.colorScheme.primary else Color.White)
                                }
                            }
                            val label = t(
                                when {
                                    recording != null -> "camera.shutter_stop"
                                    videoMode -> "camera.shutter_record"
                                    else -> "camera.shutter_photo"
                                },
                            )
                            Box(
                                Modifier
                                    .size(76.dp)
                                    .border(4.dp, Color.White, CircleShape)
                                    .padding(8.dp)
                                    .background(
                                        when {
                                            recording != null -> Color.Red
                                            saving -> Color.White.copy(alpha = 0.4f)
                                            else -> Color.White
                                        },
                                        CircleShape,
                                    )
                                    .semantics { contentDescription = label }
                                    .pointerInput(videoMode, recording) {
                                        detectTapGestures(
                                            onTap = {
                                                when {
                                                    recording != null -> stopVideo()
                                                    videoMode -> startVideo()
                                                    else -> takePhoto()
                                                }
                                            },
                                            onLongPress = { if (!videoMode && recording == null) startVideo() },
                                            onPress = {
                                                val released = tryAwaitRelease()
                                                if (released && !videoMode && recording != null) stopVideo()
                                            },
                                        )
                                    },
                            )
                        }
                    }
                }
            }
        }
    }
}
