// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.settings

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.Settings
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.core.app.NotificationManagerCompat
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.l10n.t
import me.lakko.app.push.NOTIFICATIONS_OFF_NOTICE_KEY
import me.lakko.app.push.NOTIFICATIONS_OFF_TITLE_KEY
import me.lakko.app.ui.chat.AutoplayVideoNotesSetting
import me.lakko.app.ui.components.LakkoAvatar
import me.lakko.app.ui.theme.ThemeMode
import me.lakko.app.ui.theme.ThemePreference
import me.lakko.app.ui.components.LakkoDialog
import me.lakko.app.ui.components.LakkoDialogAction
import me.lakko.app.ui.components.LakkoDialogTone
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize

/**
 * The Settings tab — docs/ux/client-redesign.md section 5.5.
 *
 * Header with the avatar, the name and the username; Account, Security
 * (Password, Passkeys, Devices, Recovery), Appearance and Media inline, Network, About
 * (version, licence) and the Danger zone at the bottom: Log out, Forget this
 * device and Delete account behind the confirmations the flows already had.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun SettingsScreen(
    displayName: String?,
    /** ADR-0084 — the own profile photo for the header, or `null`. */
    ownAvatar: me.lakko.app.profile.AvatarRefs? = null,
    username: String?,
    onOpenProfile: () -> Unit,
    onOpenPasskeys: () -> Unit,
    /** Security -> Password: choose a new password from this device. */
    onOpenPassword: () -> Unit = {},
    onOpenDevices: () -> Unit,
    onOpenRecovery: () -> Unit,
    onOpenAntiCensorship: () -> Unit,
    onOpenLanguage: () -> Unit,
    /** docs/ux/text-size.md — Settings > Appearance > Text size. */
    onOpenTextSize: () -> Unit = {},
    onLogout: () -> Unit,
    /** ADR-0059 section 6 — Security -> Channels I share. */
    onOpenChannelsIShare: () -> Unit = {},
    onForgetDevice: () -> Unit,
    /** The delete dialog; its proof and its server calls live with Passkeys. */
    onDeleteAccount: () -> Unit = {},
    modifier: Modifier = Modifier,
    /** docs/ux/chat-folders.md section 4 — between Notifications and Appearance. */
    onOpenChatFolders: () -> Unit = {},
    /** `docs/ux/who-can-find-me.md` section 2 — Settings > Privacy. */
    onOpenWhoCanFindMe: () -> Unit = {},
    /** ADR-0075 / UX 19.1 — Settings > Account: how the account is
     *  protected (status line, banner, recovery email, recovery phrase). */
    accountProtection: @Composable () -> Unit = {},
    /** reactions-and-emoji D3 / D4 — Appearance > Quick reaction: save the
     *  account's quick reaction (synced to its other devices). */
    onSetQuickReaction: (String) -> Unit = {},
) {
    val mode by ThemePreference.mode.collectAsStateWithLifecycle()
    val context = LocalContext.current
    // docs/ux/calls.md section 2.1 — whether a wake can reach this phone at
    // all. Read every time Settings is resumed, never cached at process start:
    // the row below sends the person to system settings, so the state they
    // come back to is precisely the one a cached read would still be wrong
    // about. Starts `true` so the notice does not flash before the first read.
    var notificationsEnabled by remember { mutableStateOf(true) }
    LifecycleResumeEffect(Unit) {
        notificationsEnabled = NotificationManagerCompat.from(context).areNotificationsEnabled()
        onPauseOrDispose { }
    }
    var confirmingLogout by remember { mutableStateOf(false) }
    var confirmingForget by remember { mutableStateOf(false) }
    val headerName = displayName?.takeIf { it.isNotBlank() }
        ?: username?.let { "@$it" }
        ?: t("settings.me.guest")

    Scaffold(
        modifier = modifier,
        topBar = { TopAppBar(title = { Text(t("nav.settings")) }) },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState()),
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable(onClick = onOpenProfile)
                    .padding(horizontal = 16.dp, vertical = 16.dp),
            ) {
                LakkoAvatar(name = headerName, seed = username ?: headerName, size = 56.dp, avatar = ownAvatar)
                Spacer(Modifier.width(16.dp))
                Column {
                    // A name keeps its own direction (FSI); a bare @username is LTR.
                    Text(
                        if (headerName.startsWith("@")) "\u2066$headerName\u2069" else "\u2068$headerName\u2069",
                        style = MaterialTheme.typography.titleLarge,
                    )
                    username?.let {
                        // A username is always laid out LTR (docs/ux/localization.md
                        // section 6), isolated from an RTL interface around it.
                        Text(
                            "\u2066@$it\u2069",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
            HorizontalDivider()

            // First, not filed under device behaviour lower down: this only
            // appears when it is broken, and the person reading it is the one
            // asking why their phone did not ring. Nothing is shown when
            // notifications are on — a row confirming that things work is a row
            // nobody needs.
            SectionLabel(t("settings.section.notifications"))
            if (!notificationsEnabled) {
                SettingsRow(
                    title = t(NOTIFICATIONS_OFF_TITLE_KEY),
                    icon = R.drawable.ic_lk_bell_slash,
                    onClick = { openAppNotificationSettings(context) },
                    supporting = t(NOTIFICATIONS_OFF_NOTICE_KEY),
                )
            }
            // reactions-and-emoji section 12 — the one Reactions switch.
            ReactionNotificationsRow()
            // docs/ux/channel-notifications.md section 3 — the one place the
            // cost of reading channels without a push is said.
            Text(
                t("settings.notifications.channel_posts_late"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            )

            SectionLabel(t("settings.section.account"))
            SettingsRow(t("settings.section.profile"), R.drawable.ic_lk_person_square, onOpenProfile)
            accountProtection()

            // docs/ux/who-can-find-me.md section 2 — Privacy, after Profile: it
            // is about people, not keys, which is why it is not under Security.
            SectionLabel(t("settings.section.privacy"))
            SettingsRow(t("who_can_find_me.title"), R.drawable.ic_lk_person_2_gear, onOpenWhoCanFindMe)
            ShowCityRow()

            SectionLabel(t("settings.section.security"))
            // A guest account has no password to change.
            if (username != null) {
                SettingsRow(
                    t("settings.password.title"),
                    R.drawable.ic_lk_lock,
                    onOpenPassword,
                    supporting = t("settings.password.change"),
                )
            }
            SettingsRow(t("settings.passkeys.title"), R.drawable.ic_lk_passkey, onOpenPasskeys)
            // docs/ux/identity-recovery.md 17.5 - the attention badge while a
            // new device of the account is pending.
            val deviceNotice by me.lakko.app.devices.NewDeviceNotices.view.collectAsStateWithLifecycle()
            SettingsRow(
                t("settings.devices.title"),
                R.drawable.ic_lk_devices,
                onOpenDevices,
                supporting = t("settings.security.devices.sub"),
                attention = if (deviceNotice.attention) t("settings.devices.attention_a11y") else null,
            )
            SettingsRow(
                t("settings.recovery.title"),
                R.drawable.ic_lk_lifepreserver,
                onOpenRecovery,
                supporting = t("settings.security.recovery.sub"),
            )
            SettingsRow(
                t("discover.shared.title"),
                R.drawable.ic_lk_radiowaves,
                onOpenChannelsIShare,
                supporting = t("discover.shared.settings_sub"),
            )

            SectionLabel(t("settings.section.folders"))
            SettingsRow(t("settings.section.folders"), R.drawable.ic_lk_folder, onOpenChatFolders)

            SectionLabel(t("settings.section.appearance"))
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 8.dp),
            ) {
                LkIcon(
                    R.drawable.ic_lk_appearance,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    size = LkIconSize.Row,
                )
                Spacer(Modifier.width(16.dp))
                Text(t("settings.appearance.theme"), modifier = Modifier.weight(1f))
            }
            SingleChoiceSegmentedButtonRow(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp),
            ) {
                val options = ThemeMode.entries
                options.forEachIndexed { index, option ->
                    SegmentedButton(
                        selected = mode == option,
                        onClick = { ThemePreference.set(option) },
                        shape = SegmentedButtonDefaults.itemShape(index = index, count = options.size),
                    ) {
                        Text(t(themeModeKey(option)))
                    }
                }
            }
            Spacer(Modifier.height(8.dp))
            // docs/ux/text-size.md — below Theme.
            TextSizeSettingsRow(onClick = onOpenTextSize)
            // reactions-and-emoji D4 — the quick reaction lives in Appearance.
            QuickReactionSettingRow(onSet = onSetQuickReaction)
            // docs/ux/localization.md section 1: a top-level row after Appearance.
            LanguageSettingsRow(onClick = onOpenLanguage)

            SectionLabel(t("settings.section.media"))
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 4.dp),
            ) {
                LkIcon(
                    R.drawable.ic_lk_play_circle,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    size = LkIconSize.Row,
                )
                Spacer(Modifier.width(16.dp))
                // ADR-0047 / UX spec §3 — autoplay downloads media, so it is
                // off until the user turns it on.
                AutoplayVideoNotesSetting(modifier = Modifier.weight(1f))
            }

            SectionLabel(t("settings.section.network"))
            SettingsRow(t("settings.anti_censorship.title"), R.drawable.ic_lk_network_shield, onOpenAntiCensorship)

            SectionLabel(t("settings.section.about"))
            InfoRow(
                title = t("settings.about.version"),
                value = "${me.lakko.app.BuildConfig.VERSION_NAME} (${me.lakko.app.BuildConfig.VERSION_CODE})",
                icon = R.drawable.ic_lk_info_circle,
            )
            TranslationsUpdatedRow()
            InfoRow(
                title = t("settings.about.licence"),
                value = t("settings.about.licence_body", mapOf("licence" to "AGPL-3.0-or-later")),
                icon = R.drawable.ic_lk_doc_text,
            )
            // ADR-0080 - the city database's attribution (CC BY 4.0).
            Text(
                t("settings.about.city_data"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            )

            Spacer(Modifier.height(16.dp))
            HorizontalDivider()
            SectionLabel(t("settings.section.danger_zone"), danger = true)
            SettingsRow(
                title = t("settings.account.log_out"),
                icon = R.drawable.ic_lk_logout,
                onClick = { confirmingLogout = true },
                danger = true,
            )
            SettingsRow(
                title = t("settings.account.forget_device.label"),
                icon = R.drawable.ic_lk_trash,
                onClick = { confirmingForget = true },
                danger = true,
            )
            SettingsRow(
                title = t("settings.account.delete.confirm"),
                icon = R.drawable.ic_lk_person_circle_xmark,
                onClick = onDeleteAccount,
                danger = true,
            )
            Text(
                t("settings.account.danger_zone.footer"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
            )
            Spacer(Modifier.height(24.dp))
        }
    }

    if (confirmingLogout) {
        LakkoDialog(
            onDismissRequest = { confirmingLogout = false },
            title = t("settings.account.log_out.title"),
            body = t("settings.account.log_out.body"),
            confirm = LakkoDialogAction(t("settings.account.log_out")) { confirmingLogout = false; onLogout() },
        )
    }
    if (confirmingForget) {
        LakkoDialog(
            onDismissRequest = { confirmingForget = false },
            title = t("settings.account.forget_device.title"),
            body = t("settings.account.forget_device.body_device"),
            confirm = LakkoDialogAction(t("settings.account.forget_device.confirm"), LakkoDialogTone.Danger) {
                confirmingForget = false
                onForgetDevice()
            },
        )
    }
}

/** Catalog key of a theme option's label. */
private fun themeModeKey(mode: ThemeMode): String = when (mode) {
    ThemeMode.System -> "settings.appearance.theme.system"
    ThemeMode.Light -> "settings.appearance.theme.light"
    ThemeMode.Dark -> "settings.appearance.theme.dark"
}

@Composable
private fun SectionLabel(text: String, danger: Boolean = false) {
    Text(
        text = text,
        style = MaterialTheme.typography.labelLarge,
        color = if (danger) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 16.dp, top = 16.dp, bottom = 4.dp),
    )
}

/** A read-only row (About): title on the left, value as the supporting line. */
/**
 * ADR-0061 S6 / UX localization section 7 — "Translations updated <date>" when
 * a signed translation update is active; nothing otherwise. The date is
 * formatted for the interface language, not the system one.
 */
@Composable
private fun TranslationsUpdatedRow() {
    val signedAt by me.lakko.app.l10n.Localization.translationsUpdatedAt.collectAsStateWithLifecycle()
    val millis = signedAt ?: return
    val language = me.lakko.app.l10n.LocalStrings.current.language
    val date = remember(millis, language) {
        // Words from the language, order from the device (localization.md 5).
        me.lakko.app.l10n.DeviceFormats.mediumDate(millis, language)
    }
    ListItem(
        headlineContent = { Text(t("settings.about.translations_updated", mapOf("date" to date))) },
        leadingContent = {
            LkIcon(R.drawable.ic_lk_info_circle, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant, size = LkIconSize.Row)
        },
        colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
    )
}

@Composable
private fun ShowCityRow() {
    // ADR-0080 - "Show my city to my other devices": per device, on by default.
    val on by me.lakko.app.devices.DeviceCardCenter.showCity.collectAsStateWithLifecycle()
    ListItem(
        headlineContent = { Text(t("settings.privacy.show_city")) },
        supportingContent = { Text(t("settings.privacy.show_city.sub")) },
        leadingContent = {
            LkIcon(R.drawable.ic_lk_location, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant, size = LkIconSize.Row)
        },
        trailingContent = {
            androidx.compose.material3.Switch(
                checked = on,
                onCheckedChange = { me.lakko.app.devices.DeviceCardCenter.setShowCity(it) },
            )
        },
        colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
    )
}

@Composable
private fun InfoRow(title: String, value: String, @androidx.annotation.DrawableRes icon: Int) {
    ListItem(
        headlineContent = { Text(title) },
        supportingContent = { Text(value) },
        leadingContent = { LkIcon(icon, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant, size = LkIconSize.Row) },
        colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
    )
}

@Composable
private fun SettingsRow(
    title: String,
    @androidx.annotation.DrawableRes icon: Int,
    onClick: () -> Unit,
    danger: Boolean = false,
    supporting: String? = null,
    /** The accessible name of the attention badge (UX 17.5); `null`: no badge. */
    attention: String? = null,
) {
    val tint = if (danger) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant
    ListItem(
        trailingContent = attention?.let {
            {
                me.lakko.app.ui.devices.AttentionBadge(
                    modifier = Modifier.semantics { contentDescription = it },
                )
            }
        },
        headlineContent = {
            Text(title, color = if (danger) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface)
        },
        supportingContent = supporting?.let { { Text(it) } },
        leadingContent = { LkIcon(icon, contentDescription = null, tint = tint, size = LkIconSize.Row) },
        colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.background),
        modifier = Modifier.clickable(onClick = onClick),
    )
}

/**
 * Open this app's notification settings — the only place a refused
 * `POST_NOTIFICATIONS` can be changed, since Android stops showing the prompt
 * after two dismissals.
 *
 * Falls back to the app details page if an OEM build has no per-app
 * notification screen, so the row can never be a dead end.
 */
private fun openAppNotificationSettings(context: Context) {
    val perApp = Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
        .putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName)
    try {
        context.startActivity(perApp)
        return
    } catch (_: ActivityNotFoundException) {
        // Fall through to the app details page.
    }
    val details = Intent(
        Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
        Uri.fromParts("package", context.packageName, null),
    )
    try {
        context.startActivity(details)
    } catch (_: ActivityNotFoundException) {
        // Nothing further to offer; the notice still says what is wrong.
    }
}
