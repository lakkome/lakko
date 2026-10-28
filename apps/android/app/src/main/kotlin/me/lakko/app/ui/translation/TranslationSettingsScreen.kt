// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.translation

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.Localization
import me.lakko.app.l10n.t
import me.lakko.app.translation.PackInfo
import me.lakko.app.translation.TranslationCenter
import me.lakko.app.ui.components.LakkoDialog
import me.lakko.app.ui.components.LakkoDialogAction
import me.lakko.app.ui.components.LakkoDialogTone
import me.lakko.app.ui.components.LakkoPicker
import kotlinx.coroutines.launch
import uniffi.lakko_ffi_uniffi.canonicalLanguageTag
import uniffi.lakko_ffi_uniffi.sameLanguage

/** The row on Settings > Language that opens [TranslationSettingsScreen]. */
@Composable
public fun TranslationSettingsRow(onClick: () -> Unit) {
    ListItem(
        headlineContent = { Text(t("settings.translation.title")) },
        colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
        modifier = Modifier.clickable(onClick = onClick),
    )
}

/** UX section 6 — Settings > Language > Translation and transcription. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun TranslationSettingsScreen(onBack: () -> Unit, modifier: Modifier = Modifier) {
    val settings by TranslationCenter.settings.collectAsStateWithLifecycle()
    val cap by TranslationCenter.capability.collectAsStateWithLifecycle()
    val store by TranslationCenter.store.collectAsStateWithLifecycle()
    val ui = LocalStrings.current.language
    var packs by remember { mutableStateOf<List<PackInfo>>(emptyList()) }
    var engineLanguages by remember { mutableStateOf<List<String>>(emptyList()) }
    var choosing by remember { mutableStateOf<String?>(null) } // "reading" | "target"
    var confirmDelete by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    LaunchedEffect(cap) {
        packs = TranslationCenter.packs()
        val target = settings.targetFor(ui)
        engineLanguages = (TranslationCenter.translation.sources(target) + TranslationCenter.transcription.languages())
            .mapNotNull { canonicalLanguageTag(it) }
            .distinct()
    }
    val interfaceLanguages = remember { Localization.languages.map { it.code } }

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(t("settings.translation.title")) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = t("common.back"))
                    }
                },
            )
        },
    ) { padding ->
        Column(
            Modifier
                .padding(padding)
                .fillMaxSize()
                .verticalScroll(rememberScrollState()),
        ) {
            if (cap.translation) {
                SwitchRow(t("settings.translation.show_translate"), settings.showTranslate) { on ->
                    TranslationCenter.updateSettings { it.copy(showTranslate = on) }
                }
                SectionTitle(t("settings.translation.reading"))
                ListItem(
                    headlineContent = { Text(ownLanguageName(ui)) },
                    supportingContent = { Text(languageName(ui, ui)) },
                    colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                )
                for (tag in settings.extraReading) {
                    ListItem(
                        headlineContent = { Text(ownLanguageName(tag)) },
                        supportingContent = { Text(languageName(tag, ui)) },
                        trailingContent = {
                            IconButton(onClick = {
                                TranslationCenter.updateSettings { s -> s.copy(extraReading = s.extraReading.filterNot { it == tag }) }
                            }) {
                                Icon(
                                    Icons.Filled.Close,
                                    contentDescription = t("settings.translation.reading.remove", mapOf("language" to languageName(tag, ui))),
                                )
                            }
                        },
                        colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                    )
                }
                TextButton(onClick = { choosing = "reading" }, modifier = Modifier.padding(horizontal = 8.dp)) {
                    Text(t("settings.translation.reading.add"))
                }
                // UX section 6 — `Translate into`: one value from a short
                // list, the design system's picker.
                val targetOptions = remember(engineLanguages, ui) {
                    (listOf(ui) + interfaceLanguages + engineLanguages)
                        .mapNotNull { canonicalLanguageTag(it) }
                        .distinct()
                        .sortedBy { ownLanguageName(it) }
                        .map { it to ownLanguageName(it) }
                }
                ListItem(
                    headlineContent = { Text(t("settings.translation.target")) },
                    trailingContent = {
                        LakkoPicker(
                            selected = canonicalLanguageTag(settings.targetFor(ui)) ?: settings.targetFor(ui),
                            options = targetOptions,
                            onPick = { tag ->
                                TranslationCenter.updateSettings { s ->
                                    s.copy(target = if (sameLanguage(tag, ui)) null else tag)
                                }
                            },
                            heading = t("settings.translation.target"),
                        )
                    },
                    colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                )
            } else {
                UnavailableLine(t("settings.translation.unavailable.translation"))
            }
            HorizontalDivider()
            if (cap.transcription) {
                SwitchRow(t("settings.translation.show_transcript"), settings.showTranscript) { on ->
                    TranslationCenter.updateSettings { it.copy(showTranscript = on) }
                }
            } else {
                UnavailableLine(t("settings.translation.unavailable.transcription"))
            }
            if (cap.translation || cap.transcription) {
                HorizontalDivider()
                SwitchRow(t("settings.translation.mobile_data"), settings.mobileData) { on ->
                    TranslationCenter.updateSettings { it.copy(mobileData = on) }
                }
                SectionTitle(t("settings.translation.packs"))
                if (packs.isEmpty()) {
                    UnavailableLine(t("settings.translation.packs.empty"))
                }
                for (p in packs) {
                    val size = p.sizeBytes?.let { android.text.format.Formatter.formatShortFileSize(LocalContext.current, it) }
                    val use = when {
                        p.forTranslation && p.forTranscription -> t("settings.translation.pack.for_both")
                        p.forTranslation -> t("settings.translation.pack.for_translation")
                        else -> t("settings.translation.pack.for_transcription")
                    }
                    ListItem(
                        headlineContent = { Text(ownLanguageName(p.language)) },
                        supportingContent = { Text(listOfNotNull(use, size).joinToString(" · ")) },
                        // System packs belong to Android; a translation model
                        // ML Kit downloaded for Lakko is Lakko's to remove.
                        trailingContent = {
                            if (p.managedBySystem) {
                                Text(
                                    t("settings.translation.pack.managed.android"),
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            } else {
                                TextButton(onClick = {
                                    scope.launch {
                                        TranslationCenter.removePack(p.language)
                                        packs = TranslationCenter.packs()
                                    }
                                }) {
                                    Text(t("settings.translation.pack.remove"))
                                }
                            }
                        },
                        colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                    )
                }
            }
            HorizontalDivider()
            ListItem(
                headlineContent = { Text(t("settings.translation.delete_all"), color = MaterialTheme.colorScheme.error) },
                colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                modifier = Modifier.clickable(role = Role.Button) { confirmDelete = true },
            )
            Text(
                t("settings.translation.footer"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(16.dp),
            )
        }
    }

    choosing?.let { _ ->
        val options = remember(engineLanguages, settings) {
            (interfaceLanguages + engineLanguages)
                .mapNotNull { canonicalLanguageTag(it) }
                .distinct()
                .filter { tag -> !sameLanguage(tag, ui) && settings.extraReading.none { sameLanguage(it, tag) } }
                .sortedBy { ownLanguageName(it) }
        }
        LanguageSheet(
            heading = t("settings.translation.reading.add"),
            options = options,
            onPick = { tag ->
                TranslationCenter.updateSettings { s -> s.copy(extraReading = (s.extraReading + tag).distinct()) }
                choosing = null
            },
            onDismiss = { choosing = null },
        )
    }

    if (confirmDelete) {
        val count = store.resultCount
        LakkoDialog(
            onDismissRequest = { confirmDelete = false },
            title = t("settings.translation.delete_all.title"),
            body = t("settings.translation.delete_all.body", emptyMap(), mapOf("count" to count.toULong())),
            confirm = LakkoDialogAction(
                label = t("settings.translation.delete_all.confirm"),
                tone = LakkoDialogTone.Danger,
                onClick = {
                    TranslationCenter.deleteAllResults()
                    confirmDelete = false
                },
            ),
        )
    }
}

@Composable
private fun SwitchRow(title: String, checked: Boolean, onChange: (Boolean) -> Unit) {
    ListItem(
        headlineContent = { Text(title) },
        trailingContent = { Switch(checked = checked, onCheckedChange = null) },
        colors = ListItemDefaults.colors(containerColor = Color.Transparent),
        modifier = Modifier.clickable(role = Role.Switch) { onChange(!checked) },
    )
}

@Composable
private fun SectionTitle(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 16.dp, end = 16.dp, top = 16.dp, bottom = 4.dp),
    )
}

@Composable
private fun UnavailableLine(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(16.dp),
    )
}
