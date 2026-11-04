// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.settings

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import me.lakko.app.l10n.t

/**
 * reactions-and-emoji section 6 / D4 — Settings > Appearance > Quick
 * reaction: the current emoji; a tap opens the full picker in reaction mode;
 * a pick saves (to the account's synced state, D3) and closes.
 */
@Composable
internal fun QuickReactionSettingRow(onSet: (String) -> Unit) {
    val context = LocalContext.current
    val chosen by me.lakko.app.chat.QuickReaction.chosen.collectAsStateWithLifecycle()
    val current = chosen ?: me.lakko.app.chat.QuickReaction.current()
    var picking by remember { mutableStateOf(false) }
    var dataset by remember { mutableStateOf<me.lakko.app.media.EmojiDataset?>(null) }
    LaunchedEffect(picking) {
        if (picking && dataset == null) {
            dataset = withContext(Dispatchers.IO) { me.lakko.app.media.EmojiAssets.load(context) }
        }
    }
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .clickable { picking = true }
            .padding(horizontal = 16.dp, vertical = 10.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(t("settings.appearance.quick_reaction"), modifier = Modifier.weight(1f))
            Text(current, style = MaterialTheme.typography.titleLarge)
        }
        Text(
            t("settings.appearance.quick_reaction_sub"),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
    if (picking) {
        me.lakko.app.ui.chat.EmojiPickerSheet(
            mode = me.lakko.app.ui.chat.PickerMode.REACTION,
            dataset = dataset,
            prefs = me.lakko.app.media.MediaPrefs.get(context),
            gifModel = null,
            loadGifPreview = { null },
            onEmoji = { emoji -> onSet(emoji) },
            onGif = {},
            onDismiss = { picking = false },
        )
    }
}

/**
 * reactions-and-emoji section 12 / D8 — Settings > Notifications > Reactions,
 * "When someone reacts to your messages", on by default; a device setting.
 */
@Composable
internal fun ReactionNotificationsRow() {
    val context = LocalContext.current
    var on by remember { mutableStateOf(me.lakko.app.chat.ReactionNotifyPref.isOn(context)) }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 8.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Column(Modifier.weight(1f)) {
            Text(t("settings.notifications.reactions"))
            Text(
                t("settings.notifications.reactions_sub"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Spacer(Modifier.width(12.dp))
        Switch(
            checked = on,
            onCheckedChange = {
                on = it
                me.lakko.app.chat.ReactionNotifyPref.set(context, it)
            },
        )
    }
}
