// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.profile

import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.PhotoCamera
import androidx.compose.material.icons.filled.PhotoLibrary
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.coroutines.launch
import me.lakko.app.auth.AuthRepository
import me.lakko.app.avatar.AvatarMedia
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.profile.DisplayNameCheck
import me.lakko.app.profile.InlineField
import me.lakko.app.profile.LeaveDecision
import me.lakko.app.profile.OwnProfile
import me.lakko.app.profile.PhotoEdit
import me.lakko.app.profile.UsernameCheck
import me.lakko.app.profile.checkDisplayName
import me.lakko.app.profile.checkUsername
import me.lakko.app.profile.leaveDecision
import me.lakko.app.profile.saveEnabled
import me.lakko.app.profile.usernameFieldErrorText
import me.lakko.app.ui.auth.ProofBlock
import me.lakko.app.ui.auth.ProofSurface
import me.lakko.app.ui.auth.rememberProofController
import me.lakko.app.ui.components.LakkoAvatar
import me.lakko.app.ui.components.LakkoModal

/**
 * docs/ux/profile-screen.md — Settings > Profile.
 *
 * One centred column: the avatar (112 dp) with its edit button, the name and
 * the username, each edited in place, and Save. Nothing is sent before Save;
 * Save runs the photo upload, the username change (the existing ADR-0076
 * operation, with its proof block over the screen when needed) and the
 * profile publish, in that order. Leaving with anything pending asks
 * `Save changes?` in Lakko's own dialog.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun ProfileSettingsScreen(
    /** The stored own profile, or `null` when none exists yet. */
    current: OwnProfile?,
    /** The account's canonical username, or `null` (none known here). */
    username: String?,
    repository: AuthRepository,
    deps: ProfileSaveDeps,
    onBack: () -> Unit,
    viewModel: ProfileEditViewModel = viewModel(),
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    // The first composition already shows the stored profile; later changes
    // (a save, another of the account's devices) are followed.
    if (viewModel.edit == null) viewModel.bind(current?.displayName.orEmpty(), username.orEmpty(), current?.avatar)
    LaunchedEffect(current?.displayName, current?.avatar, username) {
        viewModel.bind(current?.displayName.orEmpty(), username.orEmpty(), current?.avatar)
    }
    val edit = viewModel.edit ?: return
    val proof = rememberProofController(repository, ProofSurface.ChangeUsername)

    var leaveAsk: (() -> Unit)? by remember { mutableStateOf(null) }
    val requestLeave: (() -> Unit) -> Unit = { proceed ->
        when (leaveDecision(viewModel.edit ?: edit)) {
            LeaveDecision.LEAVE -> proceed()
            LeaveDecision.ASK -> leaveAsk = proceed
            LeaveDecision.STAY -> Unit
        }
    }
    val currentRequestLeave by rememberUpdatedState(requestLeave)
    DisposableEffect(Unit) {
        ProfileLeaveGuard.ask = { proceed -> currentRequestLeave(proceed) }
        onDispose { ProfileLeaveGuard.ask = null }
    }
    BackHandler { requestLeave(onBack) }

    var menuOpen by remember { mutableStateOf(false) }
    var mediaError: UiText? by remember { mutableStateOf(null) }
    val picker = rememberLauncherForActivityResult(ActivityResultContracts.PickVisualMedia()) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        scope.launch {
            val source = AvatarMedia.open(context, uri)
            if (source == null) {
                mediaError = UiText("profile.video.unusable")
            } else {
                mediaError = null
                viewModel.cropSource = source
            }
        }
    }
    val hasCamera = remember { me.lakko.app.media.VideoNoteRecorder.hasAnyCamera(context) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(t("settings.section.profile")) },
                navigationIcon = {
                    IconButton(onClick = { requestLeave(onBack) }) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = t("common.back"))
                    }
                },
            )
        },
    ) { padding ->
        Box(
            contentAlignment = Alignment.TopCenter,
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .imePadding(),
        ) {
            Column(
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(12.dp),
                modifier = Modifier
                    .widthIn(max = 480.dp)
                    .fillMaxWidth()
                    .verticalScroll(rememberScrollState())
                    .padding(horizontal = 16.dp, vertical = 24.dp),
            ) {
                AvatarBlock(
                    edit = edit,
                    pendingStill = viewModel.pendingStill,
                    name = edit.name.value.ifBlank { edit.username.value },
                    enabled = !edit.saving,
                    onEdit = { menuOpen = true },
                )
                mediaError?.let {
                    Text(it.text(), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
                }
                Spacer(Modifier.height(4.dp))
                NameRow(
                    field = edit.name,
                    enabled = !edit.saving,
                    onChange = { f -> viewModel.update { it.copy(name = f) } },
                )
                UsernameRow(
                    field = edit.username,
                    hasUsername = username != null,
                    enabled = !edit.saving,
                    onChange = { f -> viewModel.update { it.copy(username = f) } },
                )
                Spacer(Modifier.height(8.dp))
                Button(
                    enabled = edit.saveEnabled(),
                    onClick = { viewModel.save(deps) },
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(if (edit.saving) t("profile.save.saving") else t("common.save"))
                }
                viewModel.error?.let {
                    Text(
                        it.text(),
                        color = MaterialTheme.colorScheme.error,
                        style = MaterialTheme.typography.bodySmall,
                        textAlign = TextAlign.Center,
                    )
                }
                if (viewModel.savedLine) {
                    Text(t("profile.save.done"), style = MaterialTheme.typography.bodySmall, textAlign = TextAlign.Center)
                }
            }
        }
    }

    if (menuOpen) {
        val sheet = rememberModalBottomSheetState(skipPartiallyExpanded = true)
        ModalBottomSheet(onDismissRequest = { menuOpen = false }, sheetState = sheet) {
            Column(Modifier.navigationBarsPadding().padding(bottom = 8.dp)) {
                MenuItem(Icons.Filled.PhotoLibrary, t("profile.avatar.menu.library")) {
                    menuOpen = false
                    picker.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageAndVideo))
                }
                if (hasCamera) {
                    MenuItem(Icons.Filled.PhotoCamera, t("profile.avatar.menu.camera")) {
                        menuOpen = false
                        viewModel.cameraOpen = true
                    }
                }
                val photoSet = when (edit.photo) {
                    is PhotoEdit.New -> true
                    PhotoEdit.Removed -> false
                    PhotoEdit.Unchanged -> edit.savedAvatar != null
                }
                if (photoSet) {
                    MenuItem(Icons.Filled.Delete, t("profile.avatar.menu.remove"), destructive = true) {
                        menuOpen = false
                        viewModel.removePhoto()
                    }
                }
            }
        }
    }

    viewModel.cropSource?.let { source ->
        AvatarCropScreen(
            source = source,
            onCancel = { viewModel.cropSource = null },
            onUse = { encoded -> viewModel.usePhoto(encoded) },
        )
    }
    if (viewModel.cameraOpen) {
        ProfileCameraScreen(
            onClose = { viewModel.cameraOpen = false },
            onCaptured = { source ->
                viewModel.cameraOpen = false
                viewModel.cropSource = source
            },
        )
    }

    viewModel.proofRequest?.let {
        LakkoModal(
            onDismissRequest = { viewModel.answerProof(false) },
            title = t("username.change.title"),
            actions = {
                TextButton(onClick = { viewModel.answerProof(false) }) { Text(t("common.cancel")) }
                TextButton(
                    enabled = proof.view().mainEnabled && !proof.view().working,
                    onClick = {
                        scope.launch { if (proof.ensure(context)) viewModel.answerProof(true) }
                    },
                ) { Text(t("common.save")) }
            },
        ) {
            ProofBlock(
                controller = proof,
                onSubmit = { scope.launch { if (proof.ensure(context)) viewModel.answerProof(true) } },
                onPasskey = { scope.launch { if (proof.provePasskey(context)) viewModel.answerProof(true) } },
            )
        }
    }

    leaveAsk?.let { proceed ->
        LakkoModal(
            onDismissRequest = { leaveAsk = null },
            title = t("profile.unsaved.title"),
            actions = {
                TextButton(onClick = { leaveAsk = null }) { Text(t("profile.unsaved.cancel")) }
                TextButton(onClick = {
                    leaveAsk = null
                    viewModel.discard()
                    proceed()
                }) { Text(t("profile.unsaved.discard")) }
                TextButton(
                    enabled = edit.saveEnabled(),
                    onClick = {
                        leaveAsk = null
                        viewModel.save(deps) { outcome -> if (outcome == SaveOutcome.Saved) proceed() }
                    },
                ) { Text(t("profile.unsaved.save")) }
            },
        ) {
            Text(t("profile.unsaved.body"), style = MaterialTheme.typography.bodyMedium)
        }
    }
}

@Composable
private fun AvatarBlock(
    edit: me.lakko.app.profile.ProfileEdit,
    pendingStill: android.graphics.Bitmap?,
    name: String,
    enabled: Boolean,
    onEdit: () -> Unit,
) {
    val label = t("profile.avatar.change")
    Box(modifier = Modifier.size(112.dp)) {
        when (val photo = edit.photo) {
            is PhotoEdit.New -> LakkoAvatar(
                name = name,
                seed = name,
                size = 112.dp,
                large = true,
                localStill = pendingStill,
                localVideo = photo.video,
            )
            PhotoEdit.Removed -> LakkoAvatar(name = name, seed = name, size = 112.dp)
            PhotoEdit.Unchanged -> LakkoAvatar(
                name = name,
                seed = name,
                size = 112.dp,
                avatar = edit.savedAvatar,
                large = true,
            )
        }
        FilledTonalIconButton(
            enabled = enabled,
            onClick = onEdit,
            modifier = Modifier
                .size(32.dp)
                .align(Alignment.BottomEnd)
                .offset(x = 2.dp, y = 2.dp)
                .semantics { contentDescription = label },
        ) {
            Icon(Icons.Filled.Edit, contentDescription = null, modifier = Modifier.size(18.dp))
        }
    }
}

@Composable
private fun MenuItem(
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    label: String,
    destructive: Boolean = false,
    onClick: () -> Unit,
) {
    val color = if (destructive) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface
    ListItem(
        headlineContent = { Text(label, color = color) },
        leadingContent = { Icon(icon, contentDescription = null, tint = color) },
        colors = ListItemDefaults.colors(containerColor = androidx.compose.ui.graphics.Color.Transparent),
        modifier = Modifier.clickable(onClick = onClick),
    )
}

/** §3 — the name row. */
@Composable
private fun NameRow(field: InlineField, enabled: Boolean, onChange: (InlineField) -> Unit) {
    val check = checkDisplayName(field.value)
    val error = (check as? DisplayNameCheck.Invalid)?.problem?.text?.text()
    if (!field.open) {
        val shown = (check as? DisplayNameCheck.Valid)?.normalized
        RowWithPencil(
            text = shown ?: t("profile.name.add"),
            secondary = shown == null,
            title = true,
            pencilLabel = t("profile.name.change"),
            enabled = enabled,
            onEdit = { onChange(field.opened()) },
        )
        return
    }
    InlineTextField(
        field = field,
        valid = check is DisplayNameCheck.Valid,
        error = error,
        prefix = null,
        keyboard = KeyboardOptions(capitalization = KeyboardCapitalization.Words, imeAction = ImeAction.Done),
        transform = { it },
        onChange = onChange,
    )
}

/** §4 — the username row. */
@Composable
private fun UsernameRow(
    field: InlineField,
    hasUsername: Boolean,
    enabled: Boolean,
    onChange: (InlineField) -> Unit,
) {
    if (!hasUsername) {
        Text(
            t("profile.username.none"),
            style = MaterialTheme.typography.bodyLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        return
    }
    if (!field.open) {
        // A username is always laid out LTR (docs/ux/localization.md section 6).
        RowWithPencil(
            text = "⁦@${field.value}⁩",
            secondary = true,
            title = false,
            pencilLabel = t("profile.username.change"),
            enabled = enabled,
            onEdit = { onChange(field.opened()) },
        )
        return
    }
    val check = checkUsername(field.value)
    val unchanged = field.value == field.saved
    val error = if (unchanged) null else usernameFieldErrorText(check, field.value)?.text()
    InlineTextField(
        field = field,
        valid = unchanged || check is UsernameCheck.Valid,
        error = error,
        prefix = "@",
        keyboard = KeyboardOptions(
            capitalization = KeyboardCapitalization.None,
            autoCorrectEnabled = false,
            keyboardType = KeyboardType.Ascii,
            imeAction = ImeAction.Done,
        ),
        transform = { it.lowercase().removePrefix("@") },
        onChange = onChange,
    )
}

@Composable
private fun RowWithPencil(
    text: String,
    secondary: Boolean,
    title: Boolean,
    pencilLabel: String,
    enabled: Boolean,
    onEdit: () -> Unit,
) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Text(
            text = text,
            style = if (title) MaterialTheme.typography.titleLarge else MaterialTheme.typography.bodyLarge,
            fontWeight = if (title && !secondary) FontWeight.SemiBold else null,
            color = if (secondary) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface,
            modifier = Modifier.clickable(enabled = enabled, onClick = onEdit),
        )
        IconButton(enabled = enabled, onClick = onEdit) {
            Icon(Icons.Filled.Edit, contentDescription = pencilLabel, modifier = Modifier.size(18.dp))
        }
    }
}

/**
 * The open field of a row: focused with the text selected; closes on Done
 * or when focus leaves (stays open while invalid); hardware Escape reverts.
 */
@Composable
private fun InlineTextField(
    field: InlineField,
    valid: Boolean,
    error: String?,
    prefix: String?,
    keyboard: KeyboardOptions,
    transform: (String) -> String,
    onChange: (InlineField) -> Unit,
) {
    val focus = remember { FocusRequester() }
    var tfv by remember { mutableStateOf(TextFieldValue(field.value, TextRange(0, field.value.length))) }
    var hadFocus by remember { mutableStateOf(false) }
    val latest by rememberUpdatedState(field)
    val latestValid by rememberUpdatedState(valid)
    LaunchedEffect(Unit) { focus.requestFocus() }
    LaunchedEffect(field.value) {
        if (tfv.text != field.value) tfv = TextFieldValue(field.value, TextRange(field.value.length))
    }
    OutlinedTextField(
        value = tfv,
        onValueChange = {
            val next = transform(it.text)
            tfv = it.copy(text = next)
            onChange(latest.typed(next))
        },
        singleLine = true,
        prefix = prefix?.let { p -> { Text(p) } },
        isError = error != null,
        supportingText = error?.let { e -> { Text(e) } },
        keyboardOptions = keyboard,
        keyboardActions = KeyboardActions(onDone = { onChange(latest.closed(latestValid)) }),
        modifier = Modifier
            .fillMaxWidth()
            .focusRequester(focus)
            .onFocusChanged { state ->
                if (state.isFocused) {
                    hadFocus = true
                } else if (hadFocus) {
                    hadFocus = false
                    onChange(latest.closed(latestValid))
                }
            }
            .onPreviewKeyEvent { event ->
                if (event.key == Key.Escape && event.type == KeyEventType.KeyUp) {
                    onChange(latest.reverted())
                    true
                } else {
                    false
                }
            },
    )
}

