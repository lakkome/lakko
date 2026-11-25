// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import android.content.Context
import android.graphics.Bitmap
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import java.io.File
import java.nio.ByteBuffer
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import me.lakko.app.calls.BackgroundPhotoStore
import me.lakko.app.calls.BackgroundSelection
import me.lakko.app.calls.BackgroundStatus
import me.lakko.app.calls.CameraBackground
import me.lakko.app.calls.CameraBackgroundSetting
import me.lakko.app.calls.WebRtcPeerConnection
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.t
import me.lakko.app.ui.theme.LakkoDark
import org.webrtc.RendererCommon
import org.webrtc.SurfaceViewRenderer

/**
 * `Background` in the call's Settings sheet (`docs/ux/call-backgrounds.md`
 * section 2): a 16:9 preview of the camera as the others see it (not
 * mirrored), one horizontal strip of choices, and the hint.
 *
 * A tap applies at once; there is no Save. With the camera off the camera
 * runs for the preview only while the section is on screen (nothing is
 * sent, the camera button stays off). Own photos come from the platform
 * photo picker, the one the profile photo uses; a long-press on one offers
 * `Remove`.
 *
 * [cameraOn] is the camera button's state; [onTurnCameraOnWithout] runs
 * the failure button: no background, then the camera on.
 */
@Composable
internal fun CallBackgroundSection(
    cameraOn: Boolean,
    preferredCamera: String?,
    onTurnCameraOnWithout: () -> Unit,
) {
    val context = LocalContext.current
    remember(context) { CameraBackgroundSetting.attach(context) }
    val selection by CameraBackgroundSetting.holder.selection.collectAsState()
    if (!CameraBackground.available) {
        // This device cannot run a background: no picker (UX section 3).
        // A choice stored anyway (an older build) keeps the camera from
        // being sent; the one way out is offered.
        if (selection.isBackground) {
            Text(t("call.background.failed"), color = LakkoDark.text, fontSize = 13.sp)
            Button(
                onClick = onTurnCameraOnWithout,
                colors = ButtonDefaults.buttonColors(
                    containerColor = LakkoDark.accent,
                    contentColor = LakkoDark.accentFg,
                ),
                modifier = Modifier.heightIn(min = 48.dp),
            ) {
                Text(t("call.background.failed_action"))
            }
        }
        return
    }
    val status by CameraBackground.status.collectAsState()
    val account = me.lakko.app.contacts.AcceptedContactsStore.boundAccountB64
    val store = remember(context) { BackgroundPhotoStore.onDevice(context) }
    var photosVersion by remember { mutableIntStateOf(0) }
    val photos by produceState(emptyList<String>(), account, photosVersion) {
        value = if (account == null) emptyList() else withContext(Dispatchers.IO) { store.list(account) }
    }
    var unusable by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()

    // The preview camera, only while the call's camera is off and none runs.
    DisposableEffect(cameraOn) {
        if (!cameraOn && !CameraBackground.cameraRunning()) {
            CameraBackground.startPreviewCamera(context, preferredCamera)
        }
        onDispose { CameraBackground.stopPreviewCamera() }
    }

    val picker = rememberLauncherForActivityResult(ActivityResultContracts.PickVisualMedia()) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        val acct = account ?: return@rememberLauncherForActivityResult
        scope.launch {
            val id = withContext(Dispatchers.IO) { addPickedPhoto(context, store, acct, uri, selection) }
            if (id == null) {
                unusable = true
            } else {
                unusable = false
                photosVersion++
                CameraBackground.select(context, BackgroundSelection.Photo(id))
            }
        }
    }

    Text(
        t("call.settings.background"),
        color = LakkoDark.textMuted,
        fontSize = 13.sp,
        fontWeight = FontWeight.SemiBold,
        modifier = Modifier
            .padding(top = 10.dp)
            .semantics { heading() },
    )
    BackgroundPreview(
        cameraOn = cameraOn,
        status = status,
        selected = selection.isBackground,
        onTurnCameraOnWithout = {
            CameraBackground.stopPreviewCamera()
            onTurnCameraOnWithout()
        },
    )
    val strings = LocalStrings.current
    val builtins = remember { builtinIds() }
    LazyRow(
        modifier = Modifier
            .fillMaxWidth()
            .padding(top = 8.dp)
            .selectableGroup(),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        item {
            Thumb(
                label = strings["call.background.none"],
                spoken = strings["call.background.none"],
                selected = selection == BackgroundSelection.None,
                showLabel = true,
                onClick = { CameraBackground.select(context, BackgroundSelection.None) },
            ) { NoneGlyph() }
        }
        item {
            Thumb(
                label = strings["call.background.blur"],
                spoken = strings["call.background.blur"],
                selected = selection == BackgroundSelection.Blur(strong = false),
                showLabel = true,
                onClick = { CameraBackground.select(context, BackgroundSelection.Blur(strong = false)) },
            ) { BlurSwatch(strong = false) }
        }
        item {
            Thumb(
                label = strings["call.background.strong_blur"],
                spoken = strings["call.background.strong_blur"],
                selected = selection == BackgroundSelection.Blur(strong = true),
                showLabel = true,
                onClick = { CameraBackground.select(context, BackgroundSelection.Blur(strong = true)) },
            ) { BlurSwatch(strong = true) }
        }
        items(builtins, key = { "b:$it" }) { id ->
            val name = strings["call.background.name.$id"]
            Thumb(
                label = name,
                spoken = strings.format("call.background.builtin_label", mapOf("name" to name)),
                selected = selection == BackgroundSelection.Builtin(id),
                showLabel = false,
                onClick = { CameraBackground.select(context, BackgroundSelection.Builtin(id)) },
            ) { BuiltinThumb(id) }
        }
        items(photos, key = { "p:$it" }) { id ->
            val removeLabel = strings["call.background.remove"]
            var menuOpen by remember { mutableStateOf(false) }
            val remove = {
                menuOpen = false
                val acct = account
                if (acct != null) {
                    scope.launch {
                        withContext(Dispatchers.IO) { store.remove(acct, id) }
                        if (selection == BackgroundSelection.Photo(id)) {
                            CameraBackground.select(context, BackgroundSelection.None)
                        }
                        photosVersion++
                    }
                }
            }
            Box {
                Thumb(
                    label = strings["call.background.your_photo"],
                    spoken = strings["call.background.your_photo"],
                    selected = selection == BackgroundSelection.Photo(id),
                    showLabel = false,
                    onClick = { CameraBackground.select(context, BackgroundSelection.Photo(id)) },
                    onLongClick = { menuOpen = true },
                    actions = listOf(CustomAccessibilityAction(removeLabel) { remove(); true }),
                ) { PhotoThumb(store, account, id) }
                if (menuOpen) {
                    // The call surfaces stay dark whatever the app theme.
                    me.lakko.app.ui.theme.LakkoTheme(dark = true) {
                        me.lakko.app.ui.components.LakkoMenu(
                            rows = listOf(
                                me.lakko.app.ui.components.LakkoMenuRow(removeLabel, icon = null) { remove() },
                            ),
                            onDismissRequest = { menuOpen = false },
                            separators = false,
                        )
                    }
                }
            }
        }
        item {
            Thumb(
                label = strings["call.background.add_photo"],
                spoken = strings["call.background.add_photo"],
                selected = null,
                showLabel = false,
                onClick = {
                    picker.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
                },
            ) {
                Text("+", color = LakkoDark.text, fontSize = 26.sp, fontWeight = FontWeight.Light)
            }
        }
    }
    if (unusable) {
        Text(
            t("call.background.photo_unusable"),
            color = LakkoDark.danger,
            fontSize = 13.sp,
            modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
        )
    }
    Text(t("call.settings.background_hint"), color = LakkoDark.textMuted, fontSize = 13.sp)
}

/** The preview: what the others receive, 16:9, never mirrored. */
@Composable
private fun BackgroundPreview(
    cameraOn: Boolean,
    status: BackgroundStatus,
    selected: Boolean,
    onTurnCameraOnWithout: () -> Unit,
) {
    val label = t("call.background.preview_label")
    val egl = remember { WebRtcPeerConnection.sharedEglContext() }
    Box(
        modifier = Modifier
            .fillMaxWidth()
            .aspectRatio(16f / 9f)
            .clip(RoundedCornerShape(12.dp))
            .background(LakkoDark.sunken)
            .semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) {
        if (egl != null) {
            AndroidView(
                modifier = Modifier.fillMaxSize(),
                factory = { ctx ->
                    SurfaceViewRenderer(ctx).also { view ->
                        view.init(egl, null)
                        view.setScalingType(RendererCommon.ScalingType.SCALE_ASPECT_FILL)
                        view.setMirror(false)
                        CameraBackground.addPreviewSink(view)
                    }
                },
                onRelease = { view ->
                    CameraBackground.removePreviewSink(view)
                    view.release()
                },
            )
        }
        val failed = selected && status == BackgroundStatus.FAILED
        val starting = selected && status == BackgroundStatus.STARTING && !failed
        when {
            failed -> PreviewNote {
                Text(
                    t("call.background.failed"),
                    color = LakkoDark.text,
                    fontSize = 13.sp,
                    textAlign = TextAlign.Center,
                    modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
                )
                Button(
                    onClick = onTurnCameraOnWithout,
                    colors = ButtonDefaults.buttonColors(
                        containerColor = LakkoDark.accent,
                        contentColor = LakkoDark.accentFg,
                    ),
                    modifier = Modifier.heightIn(min = 48.dp),
                ) {
                    Text(t("call.background.failed_action"))
                }
            }
            starting -> PreviewNote {
                CircularProgressIndicator(
                    modifier = Modifier.size(20.dp),
                    color = LakkoDark.textMuted,
                    strokeWidth = 2.dp,
                )
                Text(t("call.background.preparing"), color = LakkoDark.text, fontSize = 13.sp)
            }
            !cameraOn -> Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.BottomCenter) {
                Text(
                    t("call.background.preview_camera_off"),
                    color = Color.White,
                    fontSize = 12.sp,
                    modifier = Modifier
                        .padding(8.dp)
                        .background(Color(0x99000000), RoundedCornerShape(6.dp))
                        .padding(horizontal = 8.dp, vertical = 4.dp),
                )
            }
        }
    }
}

@Composable
private fun PreviewNote(content: @Composable () -> Unit) {
    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(LakkoDark.sunken),
        contentAlignment = Alignment.Center,
    ) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier.padding(16.dp),
        ) { content() }
    }
}

/** One thumbnail: a button with a spoken name and, when [selected] is not
 *  `null`, a selected state (UX section 7). */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun Thumb(
    label: String,
    spoken: String,
    selected: Boolean?,
    showLabel: Boolean,
    onClick: () -> Unit,
    onLongClick: (() -> Unit)? = null,
    actions: List<CustomAccessibilityAction> = emptyList(),
    content: @Composable () -> Unit,
) {
    val shape = RoundedCornerShape(12.dp)
    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(4.dp),
        modifier = Modifier.width(THUMB_SIZE),
    ) {
        Box(
            modifier = Modifier
                .size(THUMB_SIZE)
                .clip(shape)
                .background(LakkoDark.sunken)
                .border(
                    width = if (selected == true) 2.dp else 1.dp,
                    color = if (selected == true) LakkoDark.accent else LakkoDark.border,
                    shape = shape,
                )
                .combinedClickable(onClick = onClick, onLongClick = onLongClick)
                .semantics {
                    contentDescription = spoken
                    role = Role.Button
                    if (selected != null) this.selected = selected
                    if (actions.isNotEmpty()) customActions = actions
                },
            contentAlignment = Alignment.Center,
        ) { content() }
        // Every thumbnail keeps the name line's height, labelled or not, so
        // the strip does not change height as it scrolls.
        Box(modifier = Modifier.height(LABEL_HEIGHT), contentAlignment = Alignment.Center) {
            if (showLabel) {
                Text(
                    label,
                    color = LakkoDark.textMuted,
                    fontSize = 11.sp,
                    maxLines = 1,
                    // The button above already speaks the name.
                    modifier = Modifier.clearAndSetSemantics { },
                )
            }
        }
    }
}

private val THUMB_SIZE = 64.dp
private val LABEL_HEIGHT = 16.dp

@Composable
private fun NoneGlyph() {
    Box(
        modifier = Modifier
            .size(28.dp)
            .border(2.dp, LakkoDark.textMuted, CircleShape),
        contentAlignment = Alignment.Center,
    ) {
        Box(
            modifier = Modifier
                .rotate(45f)
                .size(width = 2.dp, height = 30.dp)
                .background(LakkoDark.textMuted),
        )
    }
}

@Composable
private fun BlurSwatch(strong: Boolean) {
    val colours = if (strong) {
        listOf(Color(0xFF7A86B8), Color(0xFF9AA4C8), Color(0xFF8C97C0))
    } else {
        listOf(Color(0xFF6C7BB5), Color(0xFFB9C1DC), Color(0xFF5A6AA8))
    }
    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(Brush.radialGradient(colours)),
    )
}

@Composable
private fun BuiltinThumb(id: String) {
    val bitmap by produceState<Bitmap?>(null, id) {
        value = withContext(Dispatchers.Default) { renderBuiltin(id, THUMB_PIXELS, THUMB_PIXELS) }
    }
    bitmap?.let {
        Image(
            bitmap = it.asImageBitmap(),
            contentDescription = null,
            contentScale = ContentScale.Crop,
            modifier = Modifier.fillMaxSize(),
        )
    }
}

@Composable
private fun PhotoThumb(store: BackgroundPhotoStore, account: String?, id: String) {
    val bitmap by produceState<Bitmap?>(null, account, id) {
        value = withContext(Dispatchers.IO) {
            val acct = account ?: return@withContext null
            store.read(acct, id)?.let { BackgroundPhotoStore.decodeThumbnail(it, THUMB_PIXELS) }
        }
    }
    bitmap?.let {
        Image(
            bitmap = it.asImageBitmap(),
            contentDescription = null,
            contentScale = ContentScale.Crop,
            modifier = Modifier.fillMaxSize(),
        )
    }
}

private const val THUMB_PIXELS = 160

private fun builtinIds(): List<String> = try {
    uniffi.lakko_ffi_uniffi.backgroundBuiltinIds()
} catch (_: Throwable) {
    emptyList()
}

private fun renderBuiltin(id: String, width: Int, height: Int): Bitmap? = try {
    val rgba = uniffi.lakko_ffi_uniffi.backgroundBuiltinRender(id, width.toUInt(), height.toUInt())
    if (rgba == null || rgba.size != width * height * 4) {
        null
    } else {
        // ARGB_8888 is laid out R, G, B, A in memory: the core's RGBA bytes
        // copy in as they are (alpha is 255, so premultiplying changes nothing).
        Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888).apply {
            copyPixelsFromBuffer(ByteBuffer.wrap(rgba))
        }
    }
} catch (_: Throwable) {
    null
}

/** Reads the picked [uri], encodes it and stores it; the new id, or `null`
 *  when it is not a picture this device can use. */
private fun addPickedPhoto(
    context: Context,
    store: BackgroundPhotoStore,
    account: String,
    uri: Uri,
    selection: BackgroundSelection,
): String? {
    val tmp = File(context.cacheDir, "lakko-background-pick-${System.nanoTime()}.img")
    return try {
        context.contentResolver.openInputStream(uri)?.use { input ->
            tmp.outputStream().use { input.copyTo(it) }
        } ?: return null
        val jpeg = BackgroundPhotoStore.encodePicked(tmp) ?: return null
        store.add(account, jpeg, (selection as? BackgroundSelection.Photo)?.id)
    } catch (_: Throwable) {
        null
    } finally {
        tmp.delete()
    }
}

/**
 * The self-view while a background is getting ready (UX section 3): the
 * person's own avatar with a small spinner, since no picture is sent yet.
 */
@Composable
internal fun BackgroundStartingCover(modifier: Modifier = Modifier, avatarSize: androidx.compose.ui.unit.Dp = 56.dp) {
    val you = t("call.participant.you")
    val name = me.lakko.app.ui.components.rememberOwnAvatarName(you)
    Box(
        modifier = modifier.background(LakkoDark.sunken),
        contentAlignment = Alignment.Center,
    ) {
        me.lakko.app.ui.components.LakkoAvatar(
            name = name,
            size = avatarSize,
            avatar = me.lakko.app.ui.components.rememberOwnAvatar(),
        )
        CircularProgressIndicator(
            modifier = Modifier
                .align(Alignment.BottomEnd)
                .padding(8.dp)
                .size(16.dp),
            color = LakkoDark.textMuted,
            strokeWidth = 2.dp,
        )
    }
}

/** Whether the own camera tile shows [BackgroundStartingCover] now. */
@Composable
internal fun backgroundStarting(): Boolean {
    val status by CameraBackground.status.collectAsState()
    val selection by CameraBackgroundSetting.holder.selection.collectAsState()
    return selection.isBackground && status == BackgroundStatus.STARTING
}

/** The call screen's failure line (UX section 3), for a few seconds after
 *  a failure; turns the camera off through [onCameraOff] when it is on. */
@Composable
internal fun BackgroundFailureLine(
    cameraOn: Boolean,
    onCameraOff: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val currentCameraOn by androidx.compose.runtime.rememberUpdatedState(cameraOn)
    val currentOff by androidx.compose.runtime.rememberUpdatedState(onCameraOff)
    var shown by remember { mutableStateOf(false) }
    LaunchedEffect(Unit) {
        CameraBackground.failures.collect {
            if (currentCameraOn) currentOff()
            shown = true
        }
    }
    LaunchedEffect(shown) {
        if (shown) {
            kotlinx.coroutines.delay(FAILURE_LINE_MS)
            shown = false
        }
    }
    // The camera coming on again gives a failed background another chance;
    // only a change from off to on, not the screen being drawn again.
    var wasOn by remember { mutableStateOf(cameraOn) }
    LaunchedEffect(cameraOn) {
        if (cameraOn && !wasOn) CameraBackground.cameraTurnedOn()
        wasOn = cameraOn
    }
    // With the camera off no frame is processed unless the preview is open
    // (the stage's pause); back to on outside the call screen.
    DisposableEffect(cameraOn) {
        CameraBackground.cameraOn = cameraOn
        onDispose { CameraBackground.cameraOn = true }
    }
    if (shown) {
        Text(
            t("call.background.failed"),
            color = LakkoDark.text,
            fontSize = 14.sp,
            textAlign = TextAlign.Center,
            modifier = modifier
                .padding(horizontal = 24.dp)
                .background(LakkoDark.surface, RoundedCornerShape(10.dp))
                .padding(horizontal = 14.dp, vertical = 10.dp)
                .semantics { liveRegion = LiveRegionMode.Assertive },
        )
    }
}

/** How long the failure line stays on the call screen. Chosen, not measured. */
private const val FAILURE_LINE_MS = 6_000L
