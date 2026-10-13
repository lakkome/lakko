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
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Devices
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
import java.text.DateFormat
import java.util.Date
import java.util.Locale
import me.lakko.app.devices.NewDeviceNotices
import me.lakko.app.devices.newDeviceBanner
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.t

/**
 * "A device was added to your account" (`docs/ux/identity-recovery.md`
 * section 17.5): one banner at the top of the Chats list and of Settings >
 * Devices, on every device of the account except the one that was added and
 * the one that let it in. Not a modal: it must not block someone who is
 * mid-conversation, and it stays until `Dismiss` or the device's removal.
 *
 * Informational tone with a leading icon, as [me.lakko.app.ui.updates.UpdateAvailableBanner];
 * announced once when it appears (polite: it does not interrupt typing),
 * title and both lines as one announcement; `Open Devices` then `Dismiss` in
 * focus order; it never takes focus.
 *
 * [onOpenDevices] is `null` on Settings > Devices itself, where the button
 * would lead to the screen it is on.
 */
@Composable
public fun NewDeviceNoticeBanner(
    onOpenDevices: (() -> Unit)?,
    modifier: Modifier = Modifier,
) {
    val state by NewDeviceNotices.state.collectAsStateWithLifecycle()
    val banner = newDeviceBanner(state) ?: return
    val language = LocalStrings.current.language
    val title = t(if (banner.count > 1) "devices.added_notice.title_many" else "devices.added_notice.title")
    val whenLine = t("devices.added_notice.when", mapOf("date" to formatJoinTime(banner.latestAtMs, language)))
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
        Column(modifier = Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.Top,
                modifier = Modifier.semantics(mergeDescendants = true) {
                    contentDescription = spoken
                    liveRegion = LiveRegionMode.Polite
                },
            ) {
                Icon(Icons.Filled.Devices, contentDescription = null, modifier = Modifier.size(24.dp))
                Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    Text(title, style = MaterialTheme.typography.titleSmall)
                    Text(whenLine, style = MaterialTheme.typography.bodySmall)
                    Text(notYou, style = MaterialTheme.typography.bodySmall)
                }
            }
            Row(
                horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End),
                modifier = Modifier.fillMaxWidth(),
            ) {
                if (onOpenDevices != null) {
                    Button(
                        onClick = onOpenDevices,
                        modifier = Modifier.defaultMinSize(minWidth = 48.dp, minHeight = 48.dp),
                    ) { Text(t("devices.added_notice.open_devices")) }
                }
                TextButton(
                    onClick = { NewDeviceNotices.dismiss() },
                    modifier = Modifier.defaultMinSize(minWidth = 48.dp, minHeight = 48.dp),
                ) { Text(t("common.dismiss")) }
            }
        }
    }
}

/**
 * The line that replaces the banner after a device carrying `New` was
 * removed here (UX 17.5): `Device removed. If you did not add it, change
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
    DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT, Locale.forLanguageTag(language))
        .format(Date(atMs))

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
