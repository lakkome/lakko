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
import androidx.compose.material3.Button
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
import androidx.compose.ui.graphics.asImageBitmap
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
import kotlinx.coroutines.delay
import me.lakko.app.l10n.t
import java.io.File

/** UX attachments section 5 — the longest video the camera records. */
internal const val CAMERA_VIDEO_MAX_MS: Long = 60_000L

/**
 * UX attachments section 5 — Lakko's own capture view (CameraX): the shutter
 * (tap = photo; press and hold = video while held, or the Photo / Video
 * switch for a tap-only person), switch camera, flash, close. After capture
 * the take is shown with Retake and Send; Send hands the file to the same
 * pipeline as Photo or Video (a photo is re-encoded with its metadata
 * dropped). Only the permission prompt is the system's.
 */
@Composable
internal fun CameraCaptureScreen(onCaptured: (Uri) -> Unit, onClose: () -> Unit) {
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
    LaunchedEffect(Unit) { if (!permitted) askCamera.launch(Manifest.permission.CAMERA) }
    var videoMode by remember { mutableStateOf(false) }
    var back by remember { mutableStateOf(true) }
    var flash by remember { mutableStateOf(false) }
    var recording by remember { mutableStateOf<Recording?>(null) }
    var taken by remember { mutableStateOf<File?>(null) }
    val previewView = remember { PreviewView(context) }
    val imageCapture = remember { ImageCapture.Builder().build() }
    val videoCapture = remember {
        VideoCapture.withOutput(Recorder.Builder().setQualitySelector(QualitySelector.from(Quality.HD)).build())
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
        val file = File(context.cacheDir, "camera-${System.nanoTime()}.jpg")
        imageCapture.takePicture(
            ImageCapture.OutputFileOptions.Builder(file).build(),
            ContextCompat.getMainExecutor(context),
            object : ImageCapture.OnImageSavedCallback {
                override fun onImageSaved(output: ImageCapture.OutputFileResults) {
                    taken = file
                }
                override fun onError(exception: ImageCaptureException) {
                    file.delete()
                }
            },
        )
    }
    fun startVideo() {
        if (recording != null) return
        val file = File(context.cacheDir, "camera-${System.nanoTime()}.mp4")
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
                            taken = file
                        } else {
                            file.delete()
                        }
                    }
                }
        }.getOrNull()
    }
    fun stopVideo() {
        recording?.stop()
    }
    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false)) {
        Surface(color = Color.Black, modifier = Modifier.fillMaxSize()) {
            Box(Modifier.fillMaxSize().safeDrawingPadding()) {
                val shot = taken
                when {
                    denied || !permitted -> Column(
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
                    shot != null -> {
                        if (shot.extension == "jpg") {
                            val bmp = remember(shot) { android.graphics.BitmapFactory.decodeFile(shot.absolutePath) }
                            bmp?.let {
                                androidx.compose.foundation.Image(
                                    it.asImageBitmapCompat(),
                                    contentDescription = null,
                                    modifier = Modifier.fillMaxSize(),
                                )
                            }
                        } else {
                            Text(t("camera.video"), color = Color.White, modifier = Modifier.align(Alignment.Center))
                        }
                        Row(
                            horizontalArrangement = Arrangement.SpaceBetween,
                            modifier = Modifier.fillMaxWidth().align(Alignment.BottomCenter).padding(24.dp),
                        ) {
                            TextButton(onClick = { shot.delete(); taken = null }) { Text(t("camera.retake"), color = Color.White) }
                            Button(onClick = {
                                onCaptured(Uri.fromFile(shot))
                                onClose()
                            }) { Text(t("attach.preview.send")) }
                        }
                    }
                    else -> {
                        AndroidView(factory = { previewView }, modifier = Modifier.fillMaxSize())
                        Row(Modifier.fillMaxWidth().align(Alignment.TopCenter).padding(8.dp)) {
                            IconButton(onClick = onClose) { me.lakko.app.ui.components.LkIcon(me.lakko.app.R.drawable.ic_lk_xmark, t("common.close"), tint = Color.White) }
                            Box(Modifier.weight(1f))
                            IconButton(onClick = { flash = !flash }) {
                                me.lakko.app.ui.components.LkIcon(if (flash) me.lakko.app.R.drawable.ic_lk_bolt else me.lakko.app.R.drawable.ic_lk_bolt_slash, null, tint = Color.White)
                            }
                            IconButton(onClick = { back = !back }, enabled = recording == null) {
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
                                    .background(if (recording != null) Color.Red else Color.White, CircleShape)
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

private fun android.graphics.Bitmap.asImageBitmapCompat(): androidx.compose.ui.graphics.ImageBitmap =
    this.asImageBitmap()
