// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.devices

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.devices.NewDeviceNotices
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.t
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize

/**
 * "A device was added to your account" (`docs/ux/identity-recovery.md`
 * section 17.5, amended 2026-10-29): the explanation above the list of
 * Settings > Devices while a row there carries `New`. No button: the screen
 * it would open is this one, and opening it is the acknowledgement.
 *
 * Read as the first element of the screen, title and both lines as one
 * announcement; never a live region and never focused by itself.
 */
@Composable
public fun NewDeviceExplanationBlock(modifier: Modifier = Modifier) {
    val view by NewDeviceNotices.view.collectAsStateWithLifecycle()
    val explanation = view.explanation ?: return
    val language = LocalStrings.current.language
    val title = t(if (explanation.count > 1) "devices.added_notice.title_many" else "devices.added_notice.title")
    val whenLine = t("devices.added_notice.when", mapOf("date" to formatJoinTime(explanation.latestAtMs, language)))
    val notYou = t("devices.added_notice.not_you")
    val spoken = "$title. $whenLine $notYou"
    Surface(
        modifier = modifier
            .fillMaxWidth()
            .padding(8.dp),
        color = MaterialTheme.colorScheme.secondaryContainer,
        contentColor = MaterialTheme.colorScheme.onSecondaryContainer,
        shape = RoundedCornerShape(8.dp),
    ) {
        Row(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.Top,
            modifier = Modifier
                .padding(12.dp)
                .semantics(mergeDescendants = true) { contentDescription = spoken },
        ) {
            LkIcon(R.drawable.ic_lk_devices, contentDescription = null, size = LkIconSize.Toolbar)
            Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(title, style = MaterialTheme.typography.titleSmall)
                Text(whenLine, style = MaterialTheme.typography.bodySmall)
                Text(notYou, style = MaterialTheme.typography.bodySmall)
            }
        }
    }
}

/**
 * The exclamation-mark badge on the Settings destination and on the
 * `Devices` row while a new device is pending (UX 17.5), in the danger
 * colour. Never colour alone: the caller puts the accessible name
 * (`nav.settings.attention_a11y` / `settings.devices.attention_a11y`) on the
 * destination or the row it belongs to.
 */
@Composable
public fun AttentionBadge(modifier: Modifier = Modifier) {
    androidx.compose.material3.Badge(
        modifier = modifier,
        // The warning token, not the error one (maintainer, 2026-10-29): the
        // badge asks the person to look, it does not report a failure.
        containerColor = if (me.lakko.app.ui.theme.isLakkoDark()) {
            me.lakko.app.ui.theme.LakkoDark.warning
        } else {
            me.lakko.app.ui.theme.LakkoLight.warning
        },
        contentColor = if (me.lakko.app.ui.theme.isLakkoDark()) {
            me.lakko.app.ui.theme.LakkoDark.accentFg
        } else {
            me.lakko.app.ui.theme.LakkoLight.accentFg
        },
    ) {
        Text("!", fontWeight = androidx.compose.ui.text.font.FontWeight.Bold)
    }
}

/**
 * The line shown after a device carrying `New` was removed here (UX
 * 17.5): `Device removed. If you did not add it, change
 * your password now.` with `Change password`.
 */
@Composable
public fun RemovedNewDeviceLine(
    onChangePassword: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by NewDeviceNotices.state.collectAsStateWithLifecycle()
    if (!state.removedNew) return
    val message = t("settings.devices.removed_change_password")
    Surface(
        modifier = modifier
            .fillMaxWidth()
            .padding(8.dp),
        color = MaterialTheme.colorScheme.secondaryContainer,
        contentColor = MaterialTheme.colorScheme.onSecondaryContainer,
        shape = RoundedCornerShape(8.dp),
    ) {
        Column(modifier = Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(
                message,
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
            )
            Row(
                horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Button(
                    onClick = {
                        NewDeviceNotices.dismissRemoved()
                        onChangePassword()
                    },
                    modifier = Modifier.defaultMinSize(minWidth = 48.dp, minHeight = 48.dp),
                ) { Text(t("settings.password.change")) }
                TextButton(
                    onClick = { NewDeviceNotices.dismissRemoved() },
                    modifier = Modifier.defaultMinSize(minWidth = 48.dp, minHeight = 48.dp),
                ) { Text(t("common.dismiss")) }
            }
        }
    }
}

/** The join date and time, formatted by the platform in the interface
 *  language. */
internal fun formatJoinTime(atMs: Long, language: String): String =
    me.lakko.app.l10n.DeviceFormats.mediumDateTime(atMs, language)

/**
 * `docs/ux/identity-recovery.md` 18.3 - on a KEPT device after "remove and
 * change keys" ran on another one: while this device is fetching the new
 * keys, the one line `Updating keys...`; afterwards the dismissible notice
 * with `Show new phrase`, which opens Settings > Security > Recovery (that
 * screen asks for the password or the screen lock as it always does).
 *
 * Never a full-screen state and never a modal. Rendered above the chat list;
 * the app as the person left it stays underneath, with every chat.
 */
@Composable
public fun KeysRotatedNotice(
    repository: me.lakko.app.auth.AuthRepository,
    onShowPhrase: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val updating by repository.keysUpdating.collectAsStateWithLifecycle()
    val rotated by repository.keysRotatedNotice.collectAsStateWithLifecycle()
    if (updating) {
        Text(
            t("devices.rotated.updating"),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = modifier
                .padding(horizontal = 16.dp, vertical = 4.dp)
                .semantics { liveRegion = LiveRegionMode.Polite },
        )
        return
    }
    if (!rotated) return
    val notice = t("devices.rotated.notice")
    Surface(
        modifier = modifier
            .fillMaxWidth()
            .padding(8.dp),
        color = MaterialTheme.colorScheme.secondaryContainer,
        contentColor = MaterialTheme.colorScheme.onSecondaryContainer,
        shape = RoundedCornerShape(8.dp),
    ) {
        Column(modifier = Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(
                notice,
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
            )
            Row(
                horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End),
                modifier = Modifier.fillMaxWidth(),
            ) {
                // ADR-0075 - `Show new phrase` only for a phrase-protected
                // account: the others are never shown words.
                if (repository.showsNewWordsHere()) {
                    Button(
                        onClick = onShowPhrase,
                        modifier = Modifier.defaultMinSize(minWidth = 48.dp, minHeight = 48.dp),
                    ) { Text(t("devices.rotated.show_phrase")) }
                }
                TextButton(
                    onClick = { repository.dismissKeysRotatedNotice() },
                    modifier = Modifier.defaultMinSize(minWidth = 48.dp, minHeight = 48.dp),
                ) { Text(t("common.dismiss")) }
            }
        }
    }
}
