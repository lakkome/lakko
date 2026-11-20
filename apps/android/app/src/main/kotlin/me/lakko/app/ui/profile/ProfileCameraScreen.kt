// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.profile

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.view.PreviewView
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.core.content.ContextCompat
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import me.lakko.app.avatar.AvatarMedia
import me.lakko.app.avatar.ProfileCamera
import me.lakko.app.l10n.t
import me.lakko.app.profile.AvatarBounds
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize

/**
 * docs/ux/profile-screen.md §2.3 — Lakko's own camera: the front camera in a
 * circle, a switch-camera button, one shutter with a `Photo` / `Video`
 * toggle. Photo: the tap takes the picture. Video: the tap starts, a second
 * tap or 10 s stops, a ring shows the time. Either goes to the crop screen.
 * Permission refused: `Lakko can't use the camera. Allow it in your
 * settings.` with `Close`.
 */
@Composable
internal fun ProfileCameraScreen(
    onClose: () -> Unit,
    onCaptured: (AvatarMedia.Source) -> Unit,
) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    val scope = rememberCoroutineScope()
    var granted by remember {
        mutableStateOf(
            ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED,
        )
    }
    var denied by remember { mutableStateOf(false) }
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { ok ->
        granted = ok
        denied = !ok
    }
    LaunchedEffect(Unit) { if (!granted) permission.launch(Manifest.permission.CAMERA) }

    val camera = remember { ProfileCamera(context, lifecycleOwner) }
    DisposableEffect(camera) { onDispose { camera.release() } }
    var previewView: PreviewView? by remember { mutableStateOf(null) }
    var mode by remember { mutableStateOf(ProfileCamera.Mode.PHOTO) }
    var bound by remember { mutableStateOf<ProfileCamera.BindResult?>(null) }
    var recording by remember { mutableStateOf(false) }
    var elapsedMs by remember { mutableLongStateOf(0L) }
    var busy by remember { mutableStateOf(false) }
    var pendingVideo: java.io.File? by remember { mutableStateOf(null) }

    LaunchedEffect(granted, previewView) {
        val view = previewView ?: return@LaunchedEffect
        if (granted && bound == null) bound = camera.bind(view, mode)
    }

    val stop: () -> Unit = {
        if (recording && !busy) {
            busy = true
            scope.launch {
                recording = false
                val file = pendingVideo
                val ok = camera.stopVideo()
                val source = if (ok && file != null) AvatarMedia.openVideo(file) else null
                busy = false
                if (source != null) onCaptured(source)
            }
        }
    }
    LaunchedEffect(recording) {
        if (!recording) return@LaunchedEffect
        val started = System.currentTimeMillis()
        while (recording) {
            elapsedMs = System.currentTimeMillis() - started
            if (elapsedMs >= AvatarBounds.VIDEO_MAX_MS) {
                stop()
                break
            }
            delay(50)
        }
    }

    Dialog(
        onDismissRequest = { if (!recording) onClose() },
        properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
    ) {
        Surface(color = Color.Black, contentColor = Color.White, modifier = Modifier.fillMaxSize()) {
            Column(
                horizontalAlignment = Alignment.CenterHorizontally,
                modifier = Modifier.fillMaxSize().safeDrawingPadding().padding(16.dp),
            ) {
                Row(Modifier.fillMaxWidth()) {
                    IconButton(enabled = !recording, onClick = onClose) {
                        LkIcon(R.drawable.ic_lk_xmark, contentDescription = t("profile.camera.close"))
                    }
                }
                if (denied || !granted) {
                    Spacer(Modifier.weight(1f))
                    if (denied) {
                        Text(
                            t("profile.camera.denied"),
                            textAlign = TextAlign.Center,
                            style = MaterialTheme.typography.bodyLarge,
                        )
                        Spacer(Modifier.height(16.dp))
                        TextButton(onClick = onClose) { Text(t("profile.camera.close"), color = Color.White) }
                    }
                    Spacer(Modifier.weight(1f))
                    return@Column
                }
                Spacer(Modifier.weight(1f))
                Box(
                    contentAlignment = Alignment.Center,
                    modifier = Modifier.fillMaxWidth(0.85f).aspectRatio(1f).clip(CircleShape),
                ) {
                    AndroidView(
                        factory = { ctx ->
                            PreviewView(ctx).apply {
                                implementationMode = PreviewView.ImplementationMode.COMPATIBLE
                                scaleType = PreviewView.ScaleType.FILL_CENTER
                                previewView = this
                            }
                        },
                        modifier = Modifier.fillMaxSize(),
                    )
                }
                Spacer(Modifier.weight(1f))
                Row(
                    horizontalArrangement = Arrangement.spacedBy(16.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    ModeChip(t("profile.camera.photo"), mode == ProfileCamera.Mode.PHOTO, !recording && !busy) {
                        if (camera.setMode(ProfileCamera.Mode.PHOTO)) mode = ProfileCamera.Mode.PHOTO
                    }
                    ModeChip(t("profile.camera.video"), mode == ProfileCamera.Mode.VIDEO, !recording && !busy) {
                        if (camera.setMode(ProfileCamera.Mode.VIDEO)) mode = ProfileCamera.Mode.VIDEO
                    }
                }
                Spacer(Modifier.height(16.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Spacer(Modifier.width(56.dp))
                    Spacer(Modifier.width(24.dp))
                    val shutterLabel = t("profile.camera.shutter")
                    Box(
                        contentAlignment = Alignment.Center,
                        modifier = Modifier
                            .size(76.dp)
                            .semantics { contentDescription = shutterLabel }
                            .clickable(enabled = !busy && bound == ProfileCamera.BindResult.BOUND) {
                                if (mode == ProfileCamera.Mode.PHOTO) {
                                    busy = true
                                    scope.launch {
                                        val file = AvatarMedia.newFile(context, ".jpg")
                                        val ok = camera.takePhoto(file)
                                        val source = if (ok) AvatarMedia.openPhoto(file) else null
                                        busy = false
                                        if (source != null) onCaptured(source)
                                    }
                                } else if (recording) {
                                    stop()
                                } else {
                                    val file = AvatarMedia.newFile(context, ".mp4")
                                    if (camera.startVideo(file)) {
                                        pendingVideo = file
                                        elapsedMs = 0L
                                        recording = true
                                    }
                                }
                            },
                    ) {
                        if (recording) {
                            CircularProgressIndicator(
                                progress = { (elapsedMs.toFloat() / AvatarBounds.VIDEO_MAX_MS).coerceIn(0f, 1f) },
                                color = MaterialTheme.colorScheme.error,
                                strokeWidth = 4.dp,
                                modifier = Modifier.fillMaxSize(),
                            )
                        }
                        Box(
                            modifier = Modifier
                                .size(if (recording) 32.dp else 60.dp)
                                .clip(if (recording) MaterialTheme.shapes.small else CircleShape)
                                .background(if (mode == ProfileCamera.Mode.VIDEO) MaterialTheme.colorScheme.error else Color.White)
                                .border(2.dp, Color.White, CircleShape),
                        )
                    }
                    Spacer(Modifier.width(24.dp))
                    IconButton(
                        enabled = camera.canFlip && !recording && !busy,
                        onClick = { camera.flip() },
                        modifier = Modifier.size(56.dp),
                    ) {
                        LkIcon(R.drawable.ic_lk_camera_flip, contentDescription = t("profile.camera.switch"), size = LkIconSize.Toolbar)
                    }
                }
                Spacer(Modifier.height(24.dp))
            }
        }
    }
}

@Composable
private fun ModeChip(label: String, selected: Boolean, enabled: Boolean, onClick: () -> Unit) {
    TextButton(enabled = enabled, onClick = onClick) {
        Text(
            label,
            color = if (selected) MaterialTheme.colorScheme.primary else Color.White,
            style = MaterialTheme.typography.labelLarge,
        )
    }
}
