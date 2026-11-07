// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.PhotoCamera
import androidx.compose.material.icons.filled.PhotoLibrary
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
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
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import me.lakko.app.avatar.AvatarMedia
import me.lakko.app.chat.AllowedReactions
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.ui.components.LakkoAvatar
import me.lakko.app.ui.components.LakkoDialog
import me.lakko.app.ui.components.LakkoDialogAction
import me.lakko.app.ui.components.LakkoDialogTone
import me.lakko.app.ui.profile.AvatarCropScreen
import me.lakko.app.ui.profile.ProfileCameraScreen

/*
 * ADR-0087 / group-and-channel-privacy.md section 4 and reactions-and-emoji
 * section 9 — the group parts of the info screen this build adds: the header
 * photo (4.5.2), the @name row (4) and the Reactions row with its screen (9).
 */

/** What the header shows after `Use` or `Remove` (section 4.5.2). */
private sealed interface PhotoState {
    data object Idle : PhotoState
    data object Uploading : PhotoState
    data class Failed(val line: UiText) : PhotoState
}

/**
 * Section 4.5.2 — the 96 dp header avatar of the group; for whoever may set
 * the photo (owner and admins) the pencil and the in-app menu: `Photo or
 * video`, `Camera`, `Remove photo`. No Save: `Use` on the crop screen sends.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun GroupPhotoHeader(viewModel: ChatViewModel, title: String, canSet: Boolean) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val version by me.lakko.app.chat.GroupSettingsStore.version.collectAsStateWithLifecycle()
    val photo = remember(version) { viewModel.groupPhoto() }
    val owed = remember(version) { viewModel.owedGroupPhoto() }
    val imagesRevision by me.lakko.app.avatar.AvatarImages.revision.collectAsStateWithLifecycle()
    // 4.5.2 — the photo in effect is gone (expired or deleted): initials, and
    // for whoever may set it only, a line asking for another.
    val gone = remember(photo, imagesRevision) {
        photo?.avatar?.let { me.lakko.app.avatar.AvatarImages.isGone(it.still.blobIdHex) } == true
    }
    var state by remember { mutableStateOf<PhotoState>(PhotoState.Idle) }
    var menuOpen by remember { mutableStateOf(false) }
    var confirmRemove by remember { mutableStateOf(false) }
    var cropSource by remember { mutableStateOf<AvatarMedia.Source?>(null) }
    var cameraOpen by remember { mutableStateOf(false) }
    var pendingStill by remember { mutableStateOf<android.graphics.Bitmap?>(null) }
    val picker = rememberLauncherForActivityResult(ActivityResultContracts.PickVisualMedia()) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        scope.launch {
            val source = AvatarMedia.open(context, uri)
            if (source == null) {
                state = PhotoState.Failed(UiText("profile.video.unusable"))
            } else {
                cropSource = source
            }
        }
    }
    val hasCamera = remember { me.lakko.app.media.VideoNoteRecorder.hasAnyCamera(context) }
    val changeLabel = t("profile.avatar.change")
    Column(horizontalAlignment = Alignment.CenterHorizontally, modifier = Modifier.fillMaxWidth()) {
        Box(contentAlignment = Alignment.Center) {
            LakkoAvatar(
                name = title,
                seed = viewModel.groupIdHex,
                size = 96.dp,
                group = true,
                avatar = owed?.avatar ?: photo?.avatar?.takeIf { !gone },
                large = true,
                localStill = pendingStill,
                modifier = if (canSet && state != PhotoState.Uploading) {
                    Modifier.clickable { menuOpen = true }
                } else {
                    Modifier
                },
            )
            if (state == PhotoState.Uploading) {
                CircularProgressIndicator(modifier = Modifier.size(104.dp))
            }
            if (canSet) {
                Surface(
                    shape = CircleShape,
                    color = MaterialTheme.colorScheme.primary,
                    contentColor = MaterialTheme.colorScheme.onPrimary,
                    modifier = Modifier
                        .align(Alignment.BottomEnd)
                        .size(32.dp)
                        .semantics { contentDescription = changeLabel }
                        .clickable(enabled = state != PhotoState.Uploading) { menuOpen = true },
                ) {
                    Box(contentAlignment = Alignment.Center) {
                        Icon(Icons.Filled.Edit, contentDescription = null, modifier = Modifier.size(18.dp))
                    }
                }
            }
        }
        if (owed != null && state != PhotoState.Uploading) {
            Text(
                text = t("group_photo.owed"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 6.dp),
            )
        } else if (gone && canSet && state is PhotoState.Idle) {
            Text(
                text = t("group_photo.gone"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 6.dp),
            )
        }
        (state as? PhotoState.Failed)?.let { failed ->
            Text(
                text = failed.line.text(),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(top = 6.dp),
            )
        }
    }

    if (menuOpen) {
        val sheet = rememberModalBottomSheetState(skipPartiallyExpanded = true)
        ModalBottomSheet(onDismissRequest = { menuOpen = false }, sheetState = sheet) {
            Column(Modifier.navigationBarsPadding().padding(bottom = 8.dp)) {
                PhotoMenuItem(Icons.Filled.PhotoLibrary, t("profile.avatar.menu.library")) {
                    menuOpen = false
                    picker.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageAndVideo))
                }
                if (hasCamera) {
                    PhotoMenuItem(Icons.Filled.PhotoCamera, t("profile.avatar.menu.camera")) {
                        menuOpen = false
                        cameraOpen = true
                    }
                }
                if (photo?.avatar != null) {
                    PhotoMenuItem(Icons.Filled.Delete, t("profile.avatar.menu.remove"), destructive = true) {
                        menuOpen = false
                        confirmRemove = true
                    }
                }
            }
        }
    }
    if (confirmRemove) {
        LakkoDialog(
            onDismissRequest = { confirmRemove = false },
            title = t("group_photo.remove.title"),
            body = t("group_photo.remove.body"),
            confirm = LakkoDialogAction(t("group_photo.remove.confirm"), LakkoDialogTone.Danger) {
                confirmRemove = false
                scope.launch {
                    state = PhotoState.Uploading
                    state = try {
                        viewModel.removeGroupPhoto()
                        PhotoState.Idle
                    } catch (cause: Throwable) {
                        me.lakko.app.rethrowIfCancelled(cause)
                        groupPhotoFailure(cause)?.let { PhotoState.Failed(it) } ?: PhotoState.Idle
                    }
                }
            },
            cancel = LakkoDialogAction(t("common.cancel"), LakkoDialogTone.Ghost) { confirmRemove = false },
        )
    }
    cropSource?.let { source ->
        AvatarCropScreen(
            source = source,
            onCancel = { cropSource = null },
            onUse = { encoded ->
                cropSource = null
                pendingStill = encoded.stillBitmap
                scope.launch {
                    state = PhotoState.Uploading
                    state = try {
                        viewModel.uploadAndSetGroupPhoto(encoded)
                        PhotoState.Idle
                    } catch (cause: Throwable) {
                        me.lakko.app.rethrowIfCancelled(cause)
                        groupPhotoFailure(cause)?.let { PhotoState.Failed(it) } ?: PhotoState.Idle
                    } finally {
                        pendingStill = null
                    }
                }
            },
        )
    }
    if (cameraOpen) {
        ProfileCameraScreen(
            onClose = { cameraOpen = false },
            onCaptured = { source ->
                cameraOpen = false
                cropSource = source
            },
        )
    }
}

/** Section 4.5.2 — the line under the header for a failed `Use`. */
private fun groupPhotoFailure(cause: Throwable): UiText? {
    var c: Throwable? = cause
    val seen = HashSet<Throwable>()
    while (c != null && seen.add(c)) {
        if (c is me.lakko.app.chat.GroupPhotoOwed) return null
        if (c is me.lakko.app.chat.GroupPhotoNotAllowed) return UiText("group_photo.error.not_allowed")
        if (c is me.lakko.app.storage.StorageHttpError && c.status == 429) return UiText("group_photo.error.limit")
        if (c is me.lakko.app.chat.AttachmentBridgeException) return UiText("group_photo.error.upload")
        c = c.cause
    }
    return UiText("group_photo.error.upload")
}

@Composable
private fun PhotoMenuItem(
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    label: String,
    destructive: Boolean = false,
    onClick: () -> Unit,
) {
    val color = if (destructive) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(horizontal = 20.dp, vertical = 14.dp),
    ) {
        Icon(icon, contentDescription = null, tint = color)
        Spacer(Modifier.width(16.dp))
        Text(label, color = color)
    }
}

/**
 * Section 4 — the header line (`Private group` / `Public group`) and the
 * `@name` row with `Copy` (no system share sheet). Read-only here; nothing is
 * shown while this device holds no @name for the group.
 */
@Composable
internal fun GroupHandleRow(
    viewModel: ChatViewModel,
    memberCount: Int,
    isOwner: Boolean = false,
    showHandle: Boolean = true,
) {
    val context = LocalContext.current
    val version by me.lakko.app.chat.GroupSettingsStore.version.collectAsStateWithLifecycle()
    val stored = remember(version) { viewModel.groupHandle() }
    var typeOpen by remember { mutableStateOf(false) }
    if (typeOpen) {
        GroupTypeScreen(viewModel = viewModel, current = stored, onClose = { typeOpen = false })
    }
    if (stored == null) {
        // Section 4.4 — a group whose @name lapsed (or never had one): the
        // owner is asked to choose one; everyone else sees nothing.
        if (isOwner) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .background(MaterialTheme.colorScheme.primaryContainer, RoundedCornerShape(12.dp))
                    .padding(horizontal = 12.dp, vertical = 8.dp),
            ) {
                Text(t("group_info.handle.lapsed"), modifier = Modifier.weight(1f))
                TextButton(onClick = { typeOpen = true }) { Text(t("group_info.handle.choose")) }
            }
        }
        return
    }
    val handle = stored
    if (!showHandle) return
    val strings = LocalStrings.current
    if (isOwner) {
        // Section 4.1 — the owner's one row, opening Group type.
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .fillMaxWidth()
                .clickable { typeOpen = true }
                .padding(vertical = 10.dp),
        ) {
            Text(t("group_type.who_can_join"), modifier = Modifier.weight(1f))
            Text(
                t(if (handle.public) "group_type.public" else "group_type.private"),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
    Column(Modifier.fillMaxWidth().padding(vertical = 4.dp)) {
        Text(
            text = strings.format(
                if (handle.public) "group_info.header.public" else "group_info.header.private",
                counts = mapOf("count" to memberCount.toULong()),
            ),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(text = "@" + handle.handle, style = MaterialTheme.typography.bodyLarge, modifier = Modifier.weight(1f))
            IconButton(onClick = { copyPlainText(context, "@" + handle.handle) }) {
                Icon(Icons.Filled.ContentCopy, contentDescription = t("group_info.handle.copy_a11y"))
            }
        }
    }
}

private fun copyPlainText(context: android.content.Context, text: String) {
    val clipboard = context.getSystemService(android.content.Context.CLIPBOARD_SERVICE) as? android.content.ClipboardManager
    clipboard?.setPrimaryClip(android.content.ClipData.newPlainText("", text))
}

/** The Reactions row value: `All`, `Off`, or "12 reactions". */
@Composable
private fun reactionsValue(value: AllowedReactions): String {
    val strings = LocalStrings.current
    return when (value) {
        AllowedReactions.All -> strings["group_reactions.value.all"]
        AllowedReactions.Off -> strings["group_reactions.value.off"]
        is AllowedReactions.Only -> strings.format("group_reactions.value.count", counts = mapOf("count" to value.emoji.size.toULong()))
    }
}

/** Section 9 — the `Reactions` row, owner and admins only; opens the screen. */
@Composable
internal fun GroupReactionsRow(viewModel: ChatViewModel) {
    val version by me.lakko.app.chat.GroupSettingsStore.version.collectAsStateWithLifecycle()
    val current = remember(version) { viewModel.allowedReactions() }
    var open by remember { mutableStateOf(false) }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .clickable { open = true }
            .padding(vertical = 12.dp),
    ) {
        Text(t("group_reactions.title"), modifier = Modifier.weight(1f))
        Text(reactionsValue(current), color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
    if (open) {
        AllowedReactionsScreen(
            initial = current,
            onSave = { viewModel.setGroupReactions(it) },
            onClose = { open = false },
        )
    }
}

private enum class ReactionsMode { ALL, SOME, OFF }

/** Section 9 — the most a `Some reactions` list holds. */
private const val ALLOWED_REACTIONS_MAX: Int = 100

/**
 * Section 9 — the pushed `Reactions` screen: three single choices, with
 * `Some` the chosen emoji (a tap removes one; `Add` opens the picker, which
 * adds and stays open), `Save` enabled when something changed and, with
 * `Some`, at least one is chosen. Leaving with unsaved changes asks
 * "Save changes?".
 */
@OptIn(ExperimentalMaterial3Api::class, androidx.compose.foundation.layout.ExperimentalLayoutApi::class)
@Composable
internal fun AllowedReactionsScreen(
    initial: AllowedReactions,
    onSave: suspend (AllowedReactions) -> Unit,
    onClose: () -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val defaults = remember {
        try {
            uniffi.lakko_ffi_uniffi.quickReactions()
        } catch (_: Throwable) {
            listOf("👍", "❤️", "😂", "😮", "😢", "🙏")
        }
    }
    val initialMode = when (initial) {
        AllowedReactions.All -> ReactionsMode.ALL
        AllowedReactions.Off -> ReactionsMode.OFF
        is AllowedReactions.Only -> ReactionsMode.SOME
    }
    var mode by remember { mutableStateOf(initialMode) }
    var chosen by remember { mutableStateOf((initial as? AllowedReactions.Only)?.emoji ?: emptyList()) }
    var everSome by remember { mutableStateOf(initial is AllowedReactions.Only) }
    var saving by remember { mutableStateOf(false) }
    var failed by remember { mutableStateOf(false) }
    var picking by remember { mutableStateOf(false) }
    var askLeave by remember { mutableStateOf(false) }
    var dataset by remember { mutableStateOf<me.lakko.app.media.EmojiDataset?>(null) }
    LaunchedEffect(picking) {
        if (picking && dataset == null) {
            dataset = withContext(Dispatchers.IO) { me.lakko.app.media.EmojiAssets.load(context) }
        }
    }
    val value: AllowedReactions = when (mode) {
        ReactionsMode.ALL -> AllowedReactions.All
        ReactionsMode.OFF -> AllowedReactions.Off
        ReactionsMode.SOME -> AllowedReactions.Only(chosen)
    }
    val changed = value != initial
    val canSave = changed && !saving && (mode != ReactionsMode.SOME || chosen.isNotEmpty())
    val save: () -> Unit = {
        scope.launch {
            saving = true
            failed = false
            try {
                onSave(value)
                onClose()
            } catch (cause: Throwable) {
                me.lakko.app.rethrowIfCancelled(cause)
                failed = true
            } finally {
                saving = false
            }
        }
    }
    val leave: () -> Unit = { if (changed) askLeave = true else onClose() }
    Dialog(onDismissRequest = leave, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        BackHandler(onBack = leave)
        Scaffold(
            topBar = {
                TopAppBar(
                    title = { Text(t("group_reactions.title")) },
                    navigationIcon = {
                        IconButton(onClick = leave) {
                            Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = t("common.back"))
                        }
                    },
                    actions = {
                        TextButton(onClick = save, enabled = canSave) {
                            if (saving) {
                                CircularProgressIndicator(modifier = Modifier.size(18.dp))
                            } else {
                                Text(t("common.save"))
                            }
                        }
                    },
                )
            },
        ) { padding ->
            Column(
                Modifier
                    .fillMaxSize()
                    .padding(padding)
                    .verticalScroll(rememberScrollState())
                    .padding(horizontal = 16.dp, vertical = 8.dp),
            ) {
                ModeRow(mode == ReactionsMode.ALL, t("group_reactions.all"), t("group_reactions.all_sub")) {
                    mode = ReactionsMode.ALL
                }
                ModeRow(mode == ReactionsMode.SOME, t("group_reactions.some"), t("group_reactions.some_sub")) {
                    mode = ReactionsMode.SOME
                    // The first time `Some` is picked the grid starts with the
                    // six defaults.
                    if (!everSome && chosen.isEmpty()) chosen = defaults
                    everSome = true
                }
                ModeRow(mode == ReactionsMode.OFF, t("group_reactions.none"), t("group_reactions.none_sub")) {
                    mode = ReactionsMode.OFF
                }
                if (mode == ReactionsMode.SOME) {
                    Spacer(Modifier.height(12.dp))
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        for (emoji in chosen) {
                            val removeLabel = LocalStrings.current.format("group_reactions.remove_a11y", mapOf("emoji" to emoji))
                            Box(
                                contentAlignment = Alignment.Center,
                                modifier = Modifier
                                    .size(44.dp)
                                    .background(MaterialTheme.colorScheme.surfaceVariant, RoundedCornerShape(10.dp))
                                    .semantics { contentDescription = removeLabel }
                                    .clickable { chosen = chosen - emoji },
                            ) {
                                Text(emoji, style = MaterialTheme.typography.titleLarge)
                            }
                        }
                    }
                    if (chosen.isEmpty()) {
                        Text(
                            t("group_reactions.choose_one"),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.padding(top = 6.dp),
                        )
                    }
                    if (chosen.size >= ALLOWED_REACTIONS_MAX) {
                        Text(
                            t("group_reactions.max"),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.padding(top = 6.dp),
                        )
                    }
                    Spacer(Modifier.height(8.dp))
                    Button(onClick = { picking = true }, enabled = chosen.size < ALLOWED_REACTIONS_MAX) {
                        Text(t("group_reactions.add"))
                    }
                }
                if (failed) {
                    Text(
                        t("group_reactions.save_failed"),
                        color = MaterialTheme.colorScheme.error,
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }
            }
        }
    }
    if (picking) {
        // Section 9 — the picker adds and stays open (composer mode, no GIF).
        EmojiPickerSheet(
            mode = PickerMode.COMPOSER,
            dataset = dataset,
            prefs = me.lakko.app.media.MediaPrefs.get(context),
            gifModel = null,
            loadGifPreview = { null },
            onEmoji = { emoji ->
                if (emoji !in chosen && chosen.size < ALLOWED_REACTIONS_MAX) chosen = chosen + emoji
            },
            onGif = {},
            onDismiss = { picking = false },
        )
    }
    if (askLeave) {
        LakkoDialog(
            onDismissRequest = { askLeave = false },
            title = t("profile.unsaved.title"),
            confirm = LakkoDialogAction(t("common.save"), enabled = canSave) {
                askLeave = false
                save()
            },
            cancel = LakkoDialogAction(t("profile.unsaved.discard"), LakkoDialogTone.Ghost) {
                askLeave = false
                onClose()
            },
        )
    }
}

@Composable
private fun ModeRow(selected: Boolean, title: String, hint: String, onClick: () -> Unit) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(vertical = 8.dp),
    ) {
        RadioButton(selected = selected, onClick = onClick)
        Spacer(Modifier.width(8.dp))
        Column {
            Text(title)
            Text(hint, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

/**
 * Section 4.1 — the owner's Group type screen: Who can join and the @name,
 * with Save asking first (make public / make private / change @name).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun GroupTypeScreen(
    viewModel: ChatViewModel,
    current: me.lakko.app.chat.StoredGroupHandle?,
    onClose: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    val groupName by viewModel.groupName.collectAsStateWithLifecycle()
    val random = remember { me.lakko.app.chat.handleRandom() }
    var public by remember { mutableStateOf(current?.public ?: false) }
    var handle by remember {
        mutableStateOf(current?.handle ?: me.lakko.app.chat.suggestedGroupHandle(groupName, true, random))
    }
    var saving by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<UiText?>(null) }
    var confirm by remember { mutableStateOf(false) }
    val wellFormed = me.lakko.app.chat.handleWellFormed(handle)
    val needsName = public && groupName.isBlank()
    var visiblyTaken by remember { mutableStateOf(false) }
    LaunchedEffect(handle, public) {
        visiblyTaken = false
        if (!public || handle.length < 3 || !wellFormed || handle == current?.handle) return@LaunchedEffect
        kotlinx.coroutines.delay(500)
        visiblyTaken = viewModel.handleVisiblyTaken(handle) == true
    }
    val changed = current == null || current.public != public || current.handle != handle
    val canSave = changed && wellFormed && !needsName && !saving && !visiblyTaken
    val doSave: () -> Unit = {
        scope.launch {
            saving = true
            error = null
            try {
                viewModel.changeGroupType(public, handle)
                onClose()
            } catch (cause: Throwable) {
                me.lakko.app.rethrowIfCancelled(cause)
                val copy = me.lakko.app.chat.handleWriteFailure(cause)
                // Section 4.1 — offline: the change is owed and applies later.
                error = if (copy.key == "group_handle.error.offline") UiText("group_type.owed") else copy
            } finally {
                saving = false
            }
        }
    }
    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        BackHandler(onBack = onClose)
        Scaffold(
            topBar = {
                TopAppBar(
                    title = { Text(t("group_type.title")) },
                    navigationIcon = {
                        IconButton(onClick = onClose) {
                            Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = t("common.back"))
                        }
                    },
                )
            },
        ) { padding ->
            Column(
                Modifier
                    .fillMaxSize()
                    .padding(padding)
                    .verticalScroll(rememberScrollState())
                    .padding(horizontal = 16.dp, vertical = 8.dp),
            ) {
                Text(t("group_type.who_can_join"), style = MaterialTheme.typography.labelLarge)
                ModeRow(!public, t("group_type.private"), t("group_type.private_desc")) { public = false }
                ModeRow(public, t("group_type.public"), t("group_type.public_desc")) { public = true }
                Spacer(Modifier.height(8.dp))
                androidx.compose.material3.OutlinedTextField(
                    value = handle,
                    onValueChange = { handle = me.lakko.app.chat.canonicalHandleInput(it).take(32) },
                    label = { Text(t("group_type.handle")) },
                    prefix = { Text("@") },
                    singleLine = true,
                    isError = handle.isNotEmpty() && !wellFormed,
                    supportingText = {
                        Text(
                            when {
                                handle.isNotEmpty() && !wellFormed -> t("group_handle.error.bad_form")
                                visiblyTaken -> t("group_handle.error.taken")
                                public -> t("group_type.handle_public")
                                else -> t("group_type.handle_private")
                            },
                        )
                    },
                    modifier = Modifier.fillMaxWidth(),
                )
                if (needsName) {
                    Text(t("group_type.needs_name"), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
                }
                error?.let {
                    Text(it.text(), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
                }
                Spacer(Modifier.height(12.dp))
                Button(onClick = { confirm = true }, enabled = canSave, modifier = Modifier.fillMaxWidth()) {
                    Text(t(if (saving) "profile.save.saving" else "common.save"))
                }
            }
        }
    }
    if (confirm) {
        val (title, body, label) = when {
            current != null && current.public != public && public -> Triple(
                "group_type.confirm.public.title", "group_type.confirm.public.body", "group_type.confirm.public.confirm",
            )
            current != null && current.public != public -> Triple(
                "group_type.confirm.private.title", "group_type.confirm.private.body", "group_type.confirm.private.confirm",
            )
            current != null && current.handle != handle -> Triple(
                "group_type.confirm.rename.title", "group_type.confirm.rename.body", "group_type.confirm.rename.confirm",
            )
            public -> Triple(
                "group_type.confirm.public.title", "group_type.confirm.public.body", "group_type.confirm.public.confirm",
            )
            else -> Triple("group_type.title", null, "common.save")
        }
        LakkoDialog(
            onDismissRequest = { confirm = false },
            title = t(title),
            body = body?.let { t(it) },
            confirm = LakkoDialogAction(t(label)) {
                confirm = false
                doSave()
            },
            cancel = LakkoDialogAction(t("common.cancel"), LakkoDialogTone.Ghost) { confirm = false },
        )
    }
}

/**
 * ADR-0087 section 10 — a group of one: the timeline row "You created this
 * group", then "Only you are here." with `Add members` and `Copy invite
 * link`. Goes when a second person lands.
 */
@Composable
internal fun SoloGroupBlock(viewModel: ChatViewModel, onAddMembers: () -> Unit, alone: Boolean = true) {
    val context = LocalContext.current
    val created = remember(viewModel.groupIdHex) { viewModel.createdGroup() }
    if (!created && !alone) return
    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(8.dp),
        modifier = Modifier.fillMaxWidth().padding(vertical = 16.dp),
    ) {
        // The creation row stays after others join; only the block below goes.
        if (created) {
            Surface(
                shape = RoundedCornerShape(12.dp),
                color = MaterialTheme.colorScheme.surfaceVariant,
                contentColor = MaterialTheme.colorScheme.onSurfaceVariant,
            ) {
                Text(
                    t("group_solo.created"),
                    style = MaterialTheme.typography.labelMedium,
                    modifier = Modifier.padding(horizontal = 12.dp, vertical = 4.dp),
                )
            }
        }
        if (!alone) return@Column
        Text(t("group_solo.only_you"), style = MaterialTheme.typography.bodyMedium)
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(onClick = onAddMembers) { Text(t("group_solo.add_members")) }
            TextButton(onClick = { viewModel.copySoloInviteLink(context) }) { Text(t("group_solo.copy_invite")) }
        }
    }
}

/** ADR-0087 section 11 — `Delete group` for an owner alone, with the
 *  confirmation; a network failure says so and nothing changes. */
@Composable
internal fun DeleteGroupControl(viewModel: ChatViewModel) {
    val scope = rememberCoroutineScope()
    var confirming by remember { mutableStateOf(false) }
    var failed by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    val version by me.lakko.app.chat.GroupSettingsStore.version.collectAsStateWithLifecycle()
    val handle = remember(version) { viewModel.groupHandle()?.handle }.orEmpty()
    TextButton(
        onClick = { confirming = true },
        enabled = !busy,
        colors = androidx.compose.material3.ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error),
    ) { Text(t("group_delete.action")) }
    if (failed) {
        Text(t("group_handle.error.offline"), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
    }
    if (confirming) {
        LakkoDialog(
            onDismissRequest = { confirming = false },
            title = t("group_delete.title"),
            body = LocalStrings.current.format("group_delete.body", mapOf("name" to handle)),
            confirm = LakkoDialogAction(t("group_delete.confirm"), LakkoDialogTone.Danger) {
                confirming = false
                scope.launch {
                    busy = true
                    failed = false
                    try {
                        viewModel.deleteSoloGroup()
                    } catch (cause: Throwable) {
                        me.lakko.app.rethrowIfCancelled(cause)
                        failed = true
                    } finally {
                        busy = false
                    }
                }
            },
            cancel = LakkoDialogAction(t("common.cancel"), LakkoDialogTone.Ghost) { confirming = false },
        )
    }
}

/**
 * ADR-0087 4.5.3 — the avatar slot of the New group form: 96 dp, the initials
 * of the typed name (or the group glyph), the pencil and the same menu as the
 * profile photo, without `Remove photo` until one is chosen. Nothing is
 * uploaded here: the form uploads at Create.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun NewGroupPhotoSlot(
    name: String,
    chosen: AvatarMedia.Encoded?,
    enabled: Boolean,
    onChosen: (AvatarMedia.Encoded?) -> Unit,
    error: UiText?,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var menuOpen by remember { mutableStateOf(false) }
    var cropSource by remember { mutableStateOf<AvatarMedia.Source?>(null) }
    var cameraOpen by remember { mutableStateOf(false) }
    var mediaError by remember { mutableStateOf<UiText?>(null) }
    val picker = rememberLauncherForActivityResult(ActivityResultContracts.PickVisualMedia()) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        scope.launch {
            val source = AvatarMedia.open(context, uri)
            if (source == null) mediaError = UiText("profile.video.unusable") else cropSource = source
        }
    }
    val hasCamera = remember { me.lakko.app.media.VideoNoteRecorder.hasAnyCamera(context) }
    val changeLabel = t("profile.avatar.change")
    val fallback = t("chat.group.fallback_name")
    Column(horizontalAlignment = Alignment.CenterHorizontally, modifier = Modifier.fillMaxWidth().padding(top = 8.dp)) {
        Box(contentAlignment = Alignment.Center) {
            LakkoAvatar(
                name = name.ifBlank { fallback },
                seed = name.ifBlank { "new-group" },
                size = 96.dp,
                group = true,
                localStill = chosen?.stillBitmap,
                modifier = if (enabled) Modifier.clickable { menuOpen = true } else Modifier,
            )
            Surface(
                shape = CircleShape,
                color = MaterialTheme.colorScheme.primary,
                contentColor = MaterialTheme.colorScheme.onPrimary,
                modifier = Modifier
                    .align(Alignment.BottomEnd)
                    .size(32.dp)
                    .semantics { contentDescription = changeLabel }
                    .clickable(enabled = enabled) { menuOpen = true },
            ) {
                Box(contentAlignment = Alignment.Center) {
                    Icon(Icons.Filled.Edit, contentDescription = null, modifier = Modifier.size(18.dp))
                }
            }
        }
        (error ?: mediaError)?.let {
            Text(it.text(), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(top = 6.dp))
        }
    }
    if (menuOpen) {
        val sheet = rememberModalBottomSheetState(skipPartiallyExpanded = true)
        ModalBottomSheet(onDismissRequest = { menuOpen = false }, sheetState = sheet) {
            Column(Modifier.navigationBarsPadding().padding(bottom = 8.dp)) {
                PhotoMenuItem(Icons.Filled.PhotoLibrary, t("profile.avatar.menu.library")) {
                    menuOpen = false
                    picker.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageAndVideo))
                }
                if (hasCamera) {
                    PhotoMenuItem(Icons.Filled.PhotoCamera, t("profile.avatar.menu.camera")) {
                        menuOpen = false
                        cameraOpen = true
                    }
                }
                if (chosen != null) {
                    PhotoMenuItem(Icons.Filled.Delete, t("profile.avatar.menu.remove"), destructive = true) {
                        menuOpen = false
                        onChosen(null)
                    }
                }
            }
        }
    }
    cropSource?.let { source ->
        AvatarCropScreen(
            source = source,
            onCancel = { cropSource = null },
            onUse = { encoded ->
                cropSource = null
                mediaError = null
                onChosen(encoded)
            },
        )
    }
    if (cameraOpen) {
        ProfileCameraScreen(
            onClose = { cameraOpen = false },
            onCaptured = { source ->
                cameraOpen = false
                cropSource = source
            },
        )
    }
}

/** 4.5.3 — upload a chosen photo (still, and the video beside it). */
internal suspend fun uploadGroupPhoto(
    context: android.content.Context,
    encoded: AvatarMedia.Encoded,
): me.lakko.app.profile.AvatarRefs {
    val still = me.lakko.app.chat.AttachmentBridge.encryptAndUpload(bytes = encoded.still, mime = "image/jpeg", name = null)
    val stillBlob = me.lakko.app.profile.avatarBlobOf(still.blobId, still.key, still.nonce, encoded.still.size.toLong(), encoded.stillSide)
    me.lakko.app.avatar.AvatarImages.putOwn(context, stillBlob, encoded.still)
    val file = encoded.video
    val side = encoded.videoSide
    val ms = encoded.videoDurationMs
    val video = if (file != null && side != null && ms != null) {
        val bytes = withContext(Dispatchers.IO) { file.readBytes() }
        val ref = me.lakko.app.chat.AttachmentBridge.encryptAndUpload(bytes = bytes, mime = "video/mp4", name = null)
        me.lakko.app.profile.avatarBlobOf(ref.blobId, ref.key, ref.nonce, bytes.size.toLong(), side).also {
            me.lakko.app.avatar.AvatarImages.putOwn(context, it, bytes)
        }
    } else {
        null
    }
    return me.lakko.app.profile.AvatarRefs(still = stillBlob, video = video, videoDurationMs = if (video != null) ms else null)
}

/** 4.5.3 — the line under the slot for a failed upload at Create. */
internal fun newGroupPhotoFailure(cause: Throwable): UiText =
    groupPhotoFailure(cause) ?: UiText("group_photo.error.upload")
