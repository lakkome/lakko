// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.settings

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.FormatSize
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.l10n.t
import me.lakko.app.ui.theme.ChatTextScale
import me.lakko.app.ui.theme.LocalChatTextFactor
import me.lakko.app.ui.theme.TextSize
import me.lakko.app.ui.theme.TextSizePreference

/*
 * docs/ux/text-size.md — Settings > Appearance > Text size: a preview of one
 * incoming and one outgoing bubble (a reaction chip under the incoming one)
 * drawn at the chosen step, then the three steps as a single choice. A choice
 * applies at once everywhere; there is no Save.
 */

/** The Appearance row: "Text size", the current step as trailing text. */
@Composable
internal fun TextSizeSettingsRow(onClick: () -> Unit) {
    val size by TextSizePreference.size.collectAsStateWithLifecycle()
    ListItem(
        headlineContent = { Text(t("settings.text_size.title")) },
        leadingContent = {
            Icon(Icons.Filled.FormatSize, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant)
        },
        trailingContent = { Text(t(size.labelKey), color = MaterialTheme.colorScheme.onSurfaceVariant) },
        colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
        modifier = Modifier.clickable(onClick = onClick),
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun TextSizeScreen(onBack: () -> Unit, modifier: Modifier = Modifier) {
    val size by TextSizePreference.size.collectAsStateWithLifecycle()
    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(t("settings.text_size.title")) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = t("common.back"))
                    }
                },
            )
        },
    ) { padding ->
        Column(Modifier.padding(padding).fillMaxSize()) {
            TextSizePreview(size.factor)
            HorizontalDivider()
            for (step in TextSize.entries) {
                val chosen = step == size
                ListItem(
                    headlineContent = { Text(t(step.labelKey)) },
                    trailingContent = {
                        if (chosen) Icon(Icons.Filled.Check, contentDescription = null, tint = MaterialTheme.colorScheme.primary)
                    },
                    colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
                    modifier = Modifier
                        .semantics {
                            role = Role.RadioButton
                            selected = chosen
                        }
                        .clickable { TextSizePreference.set(step) },
                )
            }
        }
    }
}

/** One incoming bubble with a reaction chip under it and one outgoing
 *  bubble, at [factor]. */
@Composable
private fun TextSizePreview(factor: Float) {
    ChatTextScale(factor = factor) {
        Column(
            verticalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier.fillMaxWidth().padding(16.dp),
        ) {
            Column(horizontalAlignment = Alignment.Start, verticalArrangement = Arrangement.spacedBy(2.dp)) {
                PreviewBubble(t("settings.text_size.preview_incoming"), outgoing = false)
                PreviewChip()
            }
            Column(horizontalAlignment = Alignment.End, modifier = Modifier.fillMaxWidth()) {
                PreviewBubble(t("settings.text_size.preview_outgoing"), outgoing = true)
            }
        }
    }
}

@Composable
private fun PreviewBubble(text: String, outgoing: Boolean) {
    Surface(
        shape = RoundedCornerShape(18.dp),
        color = if (outgoing) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.surfaceVariant,
        contentColor = if (outgoing) MaterialTheme.colorScheme.onPrimary else MaterialTheme.colorScheme.onSurface,
        modifier = Modifier.widthIn(max = 280.dp),
    ) {
        Text(
            text = text,
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.padding(horizontal = 14.dp, vertical = 10.dp),
        )
    }
}

/** A chip of the chat's shape ([me.lakko.app.ui.chat.REACTION_CHIP_HEIGHT]
 *  and its siblings), scaled the way the chat scales it. */
@Composable
private fun PreviewChip() {
    val f = LocalChatTextFactor.current
    Surface(
        shape = RoundedCornerShape(percent = 50),
        color = MaterialTheme.colorScheme.surfaceVariant,
        contentColor = MaterialTheme.colorScheme.onSurface,
        modifier = Modifier.height(me.lakko.app.ui.chat.REACTION_CHIP_HEIGHT * f),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(me.lakko.app.ui.chat.REACTION_CHIP_INNER_GAP * f),
            modifier = Modifier.fillMaxHeight().padding(horizontal = me.lakko.app.ui.chat.REACTION_CHIP_PADDING * f),
        ) {
            Text(text = "👍", fontSize = me.lakko.app.ui.chat.REACTION_CHIP_EMOJI_SP, lineHeight = 22.sp)
            Text(
                text = "2",
                fontSize = me.lakko.app.ui.chat.REACTION_CHIP_COUNT_SP,
                lineHeight = 18.sp,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}
