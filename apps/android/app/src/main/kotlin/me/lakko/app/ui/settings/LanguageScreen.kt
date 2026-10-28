// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.settings

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Language
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.material3.Surface
import androidx.compose.runtime.CompositionLocalProvider
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.l10n.LanguageEntry
import me.lakko.app.l10n.Localization
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.languageNameKey
import me.lakko.app.l10n.t
import me.lakko.app.l10n.visibleLanguages

/**
 * Settings > Language (docs/ux/localization.md section 3): a search field,
 * "Same as system" first, then every supported language by its own name with
 * the translated name under it. A tap switches the whole interface at once,
 * this screen included; no confirmation, no restart.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun LanguageScreen(onBack: () -> Unit, modifier: Modifier = Modifier) {
    // ADR-0082 UX section 6 — the Translation and transcription sub-screen.
    var translationOpen by rememberSaveable { mutableStateOf(false) }
    if (translationOpen) {
        androidx.activity.compose.BackHandler { translationOpen = false }
        me.lakko.app.ui.translation.TranslationSettingsScreen(onBack = { translationOpen = false }, modifier = modifier)
        return
    }
    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(t("settings.language.title")) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = t("common.back"))
                    }
                },
            )
        },
    ) { padding ->
        Column(Modifier.padding(padding).fillMaxSize()) {
            me.lakko.app.ui.translation.TranslationSettingsRow(onClick = { translationOpen = true })
            androidx.compose.material3.HorizontalDivider()
            LanguageList(modifier = Modifier.weight(1f))
        }
    }
}

/**
 * The same list as a dialog, for the auth screens, where a person who cannot
 * read English must be able to switch before creating an account.
 */
@Composable
public fun LanguagePickerDialog(onDismiss: () -> Unit) {
    Dialog(onDismissRequest = onDismiss, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        // A dialog is a separate window: carry the language's direction into it.
        val rtl = LocalStrings.current.rtl
        CompositionLocalProvider(
            LocalLayoutDirection provides if (rtl) LayoutDirection.Rtl else LayoutDirection.Ltr,
        ) {
            Surface(
                shape = MaterialTheme.shapes.extraLarge,
                modifier = Modifier
                    .fillMaxWidth(0.92f)
                    .heightIn(max = 600.dp),
            ) {
                Column {
                    Text(
                        t("settings.language.title"),
                        style = MaterialTheme.typography.headlineSmall,
                        modifier = Modifier.padding(start = 24.dp, end = 24.dp, top = 24.dp, bottom = 8.dp),
                    )
                    LanguageList(modifier = Modifier.weight(1f, fill = false))
                    TextButton(
                        onClick = onDismiss,
                        modifier = Modifier.align(Alignment.End).padding(8.dp),
                    ) { Text(t("common.close")) }
                }
            }
        }
    }
}

/**
 * The trigger on the auth screens: globe icon + the current language's own
 * name; the accessibility label is `auth.language.button`.
 */
@Composable
public fun AuthLanguageButton(modifier: Modifier = Modifier) {
    val state by Localization.state.collectAsStateWithLifecycle()
    var open by rememberSaveable { mutableStateOf(false) }
    val label = t("auth.language.button")
    TextButton(
        onClick = { open = true },
        modifier = modifier.semantics { contentDescription = label },
    ) {
        Icon(Icons.Filled.Language, contentDescription = null)
        Spacer(Modifier.width(8.dp))
        Text(Localization.ownName(state.resolved))
    }
    if (open) LanguagePickerDialog(onDismiss = { open = false })
}

@Composable
private fun LanguageList(modifier: Modifier = Modifier) {
    val state by Localization.state.collectAsStateWithLifecycle()
    val strings = LocalStrings.current
    var query by rememberSaveable { mutableStateOf("") }
    val rows = remember(query, strings) {
        visibleLanguages(
            entries = Localization.languages,
            translatedName = { strings[languageNameKey(it.code)] },
            uiLanguage = strings.language,
            query = query,
        )
    }
    Column(modifier = modifier) {
        OutlinedTextField(
            value = query,
            onValueChange = { query = it },
            singleLine = true,
            placeholder = { Text(t("settings.language.search")) },
            leadingIcon = { Icon(Icons.Filled.Search, contentDescription = null) },
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 16.dp, vertical = 8.dp),
        )
        LazyColumn(modifier = Modifier.fillMaxWidth()) {
            if (query.isBlank()) {
                item(key = "system") {
                    LanguageRow(
                        title = t("settings.language.same_as_system"),
                        subtitle = t(
                            "settings.language.same_as_system_detail",
                            mapOf("language" to Localization.ownName(state.systemResolved)),
                        ),
                        selected = state.selected == null,
                        onClick = { Localization.select(null) },
                    )
                }
            }
            if (rows.isEmpty()) {
                item(key = "empty") {
                    Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) {
                        Text(
                            t("settings.language.no_results"),
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            textAlign = TextAlign.Center,
                        )
                    }
                }
            }
            items(rows, key = { it.code }) { entry ->
                LanguageRow(
                    title = entry.ownName,
                    subtitle = strings[languageNameKey(entry.code)],
                    selected = state.selected == entry.code,
                    onClick = { Localization.select(entry.code) },
                    titleRtl = entry.rtl,
                )
            }
        }
    }
}

@Composable
private fun LanguageRow(
    title: String,
    subtitle: String,
    selected: Boolean,
    onClick: () -> Unit,
    titleRtl: Boolean? = null,
) {
    ListItem(
        headlineContent = {
            // The own name keeps its own direction whatever the UI direction.
            if (titleRtl == null) {
                Text(title)
            } else {
                Text("⁨$title⁩")
            }
        },
        supportingContent = { Text(subtitle) },
        trailingContent = {
            if (selected) {
                Icon(Icons.Filled.Check, contentDescription = null, tint = MaterialTheme.colorScheme.primary)
            }
        },
        colors = ListItemDefaults.colors(containerColor = androidx.compose.ui.graphics.Color.Transparent),
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .semantics(mergeDescendants = true) {},
    )
}

/** The Settings row that opens [LanguageScreen]; trailing = current language's own name. */
@Composable
internal fun LanguageSettingsRow(onClick: () -> Unit) {
    val state by Localization.state.collectAsStateWithLifecycle()
    ListItem(
        headlineContent = { Text(t("settings.language.title")) },
        leadingContent = {
            Icon(Icons.Filled.Language, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant)
        },
        trailingContent = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "⁨${Localization.ownName(state.resolved)}⁩",
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        },
        colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
        modifier = Modifier.clickable(onClick = onClick),
    )
}
