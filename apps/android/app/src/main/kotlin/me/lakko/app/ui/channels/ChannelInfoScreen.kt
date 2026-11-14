// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import android.util.Log
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import me.lakko.app.channels.ChannelDiscovery
import me.lakko.app.channels.ChannelHandle
import me.lakko.app.channels.ChannelMetaPublisher
import me.lakko.app.channels.ChannelPublisherLoop
import me.lakko.app.channels.ChannelsClient
import me.lakko.app.channels.ChannelsComposer
import me.lakko.app.channels.ChannelsUiState
import me.lakko.app.channels.channelHeaderLineKey
import me.lakko.app.channels.fullRecord
import me.lakko.app.channels.loadChannelMembers
import me.lakko.app.channels.sharedMeta
import me.lakko.app.channels.subscriberBand
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.reach.ReachManager
import me.lakko.app.reach.reachHex
import me.lakko.app.ui.components.LakkoAvatar
import me.lakko.app.ui.components.LakkoDialog
import me.lakko.app.ui.components.LakkoDialogAction
import me.lakko.app.ui.components.LakkoDialogTone
import me.lakko.app.ui.contacts.copyToClipboard
import me.lakko.app.profile.toFfi
import me.lakko.app.chat.toFfiAllowed
import me.lakko.app.ui.settings.toModel
import uniffi.lakko_ffi_uniffi.InfoSection
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize

private const val INFO_TAG = "ChannelInfo"

private enum class InfoPage { Info, ChannelType, Publishers, InviteLink, Subscribers, Reactions }

/**
 * ADR-0087 section 12.1 / UX 12.1 — the info screen of a channel: header
 * (avatar, name, `@name`, the line, the description), the actions, the
 * detail, the exit block. Which blocks show is asked of
 * `lakko_ui_rules::conversation_info_sections` over one capability record, the
 * group's function, so a channel cannot grow a screen of its own; a block
 * this build does not draw yet is skipped.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun ChannelInfoScreen(
    channelId: String,
    state: ChannelsUiState,
    client: ChannelsClient,
    onBack: () -> Unit,
    /** The channel is gone from this device (unsubscribed). */
    onLeft: () -> Unit,
    /** Section 14.1 — the contact picker's sources; `null` hides the row. */
    chatRepository: me.lakko.app.chat.ChatRepository? = null,
    contactsRepository: me.lakko.app.contacts.ContactsRepository? = null,
) {
    val context = LocalContext.current
    val strings = LocalStrings.current
    val scope = rememberCoroutineScope()
    val channels by state.channels.collectAsState()
    val keys by state.keysByChannel.collectAsState()
    val channel = channels.firstOrNull { it.id == channelId }
    val loop = remember(state) { ChannelPublisherLoop.peek(state) }
    val counts by (loop?.subscriberCounts ?: kotlinx.coroutines.flow.MutableStateFlow(emptyMap())).collectAsState()
    val publisher = keys.containsKey(channelId)
    val owner = publisher && state.accountRoleOf(channelId) == me.lakko.app.channels.ChannelAccountRole.OWNER
    // docs/ux/second-device.md section 4a — the account owns or co-publishes
    // the channel from another device: a subscriber's screen with the exit
    // block empty.
    val keysReady by state.publisherKeysReady.collectAsState()
    val orphanedSet by state.orphaned.collectAsState()
    val readerRole = me.lakko.app.channels.readerDeviceRole(
        role = remember(channels, keys, channelId) { state.accountRoleOf(channelId) },
        publishesHere = publisher && channelId !in orphanedSet,
        orphaned = channelId in orphanedSet,
        keysReady = keysReady,
    )
    val record = remember(channel) { channel?.sharedMeta()?.fullRecord() }
    // Section 12 — unknown visibility shows no header line; the owner's
    // controls (which always hold the record) read it as known.
    val knownPublic = remember(channel, channelId) {
        me.lakko.app.channels.channelVisibilityOf(context, state.boundAccount, channel, channelId)
    }
    val public = knownPublic ?: false
    val handle = record?.handle?.handle ?: channel?.handle?.takeIf { it.isNotBlank() }
    var page by remember(channelId) { mutableStateOf(InfoPage.Info) }
    var confirmUnsubscribe by remember { mutableStateOf(false) }
    /** Section 14.1 / 15.2 — the contact picker: subscribers or a publisher. */
    var inviting by remember { mutableStateOf<Boolean?>(null) }
    var publicLink by remember(channelId) { mutableStateOf<me.lakko.app.channels.ChannelLinkMint?>(null) }

    if (channel == null) {
        LaunchedEffect(Unit) { onBack() }
        return
    }
    val title = channelTitle(channel, strings)
    val count = counts[channelId]
    val headerLine = channelHeaderLineKey(knownPublic, count?.let(::subscriberBand))
        ?.let { (key, args) -> strings.format(key, args) }

    BackHandler(enabled = page != InfoPage.Info) { page = InfoPage.Info }

    // who-can-find-me section 5 — the switch's row appears only when Members
    // of my groups and channels is on (the row itself reads the same).
    val reach = remember { me.lakko.app.reach.ReachManager.get(context) }
    val visibility by reach.visibility.collectAsState()
    val membersOn = visibility?.let {
        val model = it.toModel()
        model.mode == me.lakko.app.ui.settings.FindMeMode.SOME && model.groupsForm != null
    } ?: false
    // Section 15 (phase B) — the publisher set the loop reads off the
    // PublisherGroup's members on every pass.
    val publisherLoop = remember(state) { ChannelPublisherLoop.peek(state) }
    val publisherSets by (publisherLoop?.publishers
        ?: kotlinx.coroutines.flow.MutableStateFlow(emptyMap())).collectAsState()
    val hasOtherPublishers = publisherSets[channelId]?.any { !it.owner } ?: true
    var confirmDelete by remember { mutableStateOf(false) }
    val deleteScope = rememberCoroutineScope()
    var deleteError by remember { mutableStateOf<UiText?>(null) }
    val sections = remember(owner, publisher, handle, membersOn, hasOtherPublishers, readerRole) {
        try {
            me.lakko.app.channels.readerDeviceSections(readerRole, uniffi.lakko_ffi_uniffi.channelInfoSections(
                uniffi.lakko_ffi_uniffi.ChannelInfoCapabilities(
                    role = when {
                        owner -> uniffi.lakko_ffi_uniffi.ChannelRole.OWNER
                        publisher -> uniffi.lakko_ffi_uniffi.ChannelRole.PUBLISHER
                        else -> uniffi.lakko_ffi_uniffi.ChannelRole.SUBSCRIBER
                    },
                    hasHandle = handle != null,
                    hasOtherPublishers = hasOtherPublishers,
                    membersOfGroupsAndChannelsOn = membersOn,
                ),
            ))
        } catch (_: Throwable) {
            emptyList()
        }
    }

    // reactions-and-emoji section 9 — the owner's Reactions screen, pushed.
    if (page == InfoPage.Reactions) {
        me.lakko.app.ui.chat.AllowedReactionsScreen(
                    initial = record?.reactions?.let { me.lakko.app.chat.allowedReactionsOf(it) }
                        ?: me.lakko.app.chat.AllowedReactions.All,
                    onSave = { value ->
                        val shared = channel.sharedMeta()
                        ChannelMetaPublisher.post(
                            state = state,
                            client = client,
                            channelId = channelId,
                            name = shared?.name ?: channel.localName,
                            description = shared?.description ?: channel.localDescription,
                            updatedAtMs = System.currentTimeMillis().toULong(),
                            reactions = value.toFfiAllowed(),
                        )
                    },
                    onClose = { page = InfoPage.Info },
                )
        return
    }
    Scaffold(
        topBar = {
            TopAppBar(
                navigationIcon = {
                    IconButton(onClick = { if (page == InfoPage.Info) onBack() else page = InfoPage.Info }) {
                        LkIcon(R.drawable.ic_lk_chevron_back, contentDescription = t("common.back"))
                    }
                },
                title = {
                    Text(
                        when (page) {
                            InfoPage.Info -> title
                            InfoPage.ChannelType -> t("channel_type.title")
                            InfoPage.Publishers -> t("channels.info.publishers")
                            InfoPage.InviteLink -> t("chat.invite_link.label")
                            InfoPage.Subscribers -> count?.let {
                                t("channels.subscribers.title_count", mapOf("count" to it.toString()))
                            } ?: t("channels.subscribers.title")
                            InfoPage.Reactions -> t("group_reactions.title")
                        },
                    )
                },
            )
        },
    ) { inner ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(inner)
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            when (page) {
                InfoPage.Info -> {
                    // Section 4.4 — a lapsed @name: the owner is asked for one.
                    if (owner && handle == null) {
                        Text(
                            t("channels.info.handle_lapsed"),
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.primary,
                            modifier = Modifier
                                .fillMaxWidth()
                                .clickable { page = InfoPage.ChannelType }
                                .padding(vertical = 8.dp),
                        )
                    }
                    InfoHeader(
                        channel = channel,
                        title = title,
                        handle = handle.takeIf { InfoSection.HANDLE in sections },
                        line = headerLine,
                        avatar = record?.avatar?.let { me.lakko.app.profile.avatarRefsOf(it) },
                        canRename = InfoSection.RENAME_GROUP in sections,
                        state = state,
                        client = client,
                    )
                    for (section in sections) {
                        when (section) {
                            InfoSection.INVITE_LINK -> InfoRow(t("chat.invite_link.label")) { page = InfoPage.InviteLink }
                            InfoSection.INVITE_PEOPLE -> if (chatRepository != null && contactsRepository != null) {
                                InfoRow(t("channels.info.invite_people")) { inviting = false }
                            }
                            InfoSection.SUBSCRIBERS -> InfoRow(
                                t("channels.subscribers.title"),
                                trailing = count?.let { if (publisher) it.toString() else subscriberBand(it) },
                            ) { page = InfoPage.Subscribers }
                            InfoSection.REACTIONS -> InfoRow(t("group_reactions.title")) { page = InfoPage.Reactions }
                            InfoSection.ADMINS -> InfoRow(t("channels.info.publishers")) { page = InfoPage.Publishers }
                            InfoSection.GROUP_TYPE -> InfoRow(t("channel_type.title"), trailing = if (public) t("group_type.public") else t("group_type.private")) {
                                page = InfoPage.ChannelType
                            }
                            InfoSection.DELETE_GROUP -> {
                                HorizontalDivider()
                                Text(
                                    t("channels.delete.control"),
                                    color = MaterialTheme.colorScheme.error,
                                    style = MaterialTheme.typography.bodyLarge,
                                    modifier = Modifier
                                        .fillMaxWidth()
                                        .clickable { confirmDelete = true }
                                        .padding(vertical = 14.dp),
                                )
                                deleteError?.let {
                                    Text(it.text(), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
                                }
                            }
                            InfoSection.UNSUBSCRIBE -> {
                                HorizontalDivider()
                                Text(
                                    t("channels.unsubscribe.action"),
                                    color = MaterialTheme.colorScheme.error,
                                    style = MaterialTheme.typography.bodyLarge,
                                    modifier = Modifier
                                        .fillMaxWidth()
                                        .clickable { confirmUnsubscribe = true }
                                        .padding(vertical = 14.dp),
                                )
                            }
                            // who-can-find-me section 5 — the row draws
                            // nothing unless the setting includes channels.
                            InfoSection.MEMBERS_CAN_FIND_ME -> me.lakko.app.ui.settings.MembersCanFindMeRow(
                                reach = me.lakko.app.reach.ReachManager.get(context),
                                kind = me.lakko.app.ui.settings.FindMeScopeKind.CHANNEL,
                                idHex = channelId,
                                modifier = Modifier.fillMaxWidth(),
                            )
                            else -> Unit
                        }
                    }
                }
                InfoPage.ChannelType -> ChannelTypePage(
                    channel = channel,
                    public = public,
                    handle = handle.orEmpty(),
                    state = state,
                    client = client,
                    onDone = { page = InfoPage.Info },
                )
                InfoPage.Publishers -> PublishersPage(
                    channelId = channelId,
                    owner = owner,
                    state = state,
                    channelName = title,
                    nameOf = { key ->
                        contactsRepository?.state?.value?.accepted
                            ?.firstOrNull { it.request.fromPubkey.contentEquals(key) }
                            ?.displayName?.takeIf { it.isNotBlank() }
                            ?: key.take(4).joinToString("") { "%02x".format(it) }
                    },
                    onSteppedDown = { page = InfoPage.Info },
                    onAddPublisher = if (chatRepository != null && contactsRepository != null) {
                        { inviting = true }
                    } else {
                        null
                    },
                )
                InfoPage.Subscribers -> SubscribersPage(
                    channelId = channelId,
                    state = state,
                    client = client,
                    count = count,
                    canRemove = publisher && !public,
                )
                InfoPage.Reactions -> Unit
                InfoPage.InviteLink -> InviteLinkPage(
                    channel = channel,
                    public = public,
                    state = state,
                    client = client,
                )
            }
        }
    }

    val picking = inviting
    // Sections 14.1 and 15.2 — the people known to be in the channel are not
    // offered: the owner and the publishers, and for `Invite people` the
    // subscribers whose card this device could open.
    var cardKeysHex by remember(channelId) { mutableStateOf(emptySet<String>()) }
    LaunchedEffect(channelId, picking) {
        if (picking != false) return@LaunchedEffect
        cardKeysHex = try {
            loadChannelMembers(
                channelIdHex = channelId,
                seedForEpoch = { epoch -> me.lakko.app.channels.ChannelMemberCards.seedForEpoch(state, channelId, epoch) },
                ownSubscriberPkHex = state.subscriberKeysByChannel.value[channelId]?.publicKey?.let(::reachHex),
                ports = me.lakko.app.channels.ChannelMemberCards.ports(ReachManager.get(context), client),
            ).rows.map { it.key.lowercase() }.filter { it.length == 64 && it.all { c -> c in "0123456789abcdef" } }.toSet()
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (_: Throwable) {
            emptySet()
        }
    }
    val inChannelHex = channelInviteExclusions(
        publisherKeys = publisherSets[channelId].orEmpty().map { it.accountKey },
        cardKeysHex = if (picking == false) cardKeysHex else emptySet(),
    )
    if (picking != null && chatRepository != null && contactsRepository != null) {
        ChannelInviteContactsDialog(
            inChannelHex = inChannelHex,
            chatRepository = chatRepository,
            contactsRepository = contactsRepository,
            linkFor = { _ ->
                try {
                    if (picking) {
                        // Section 15.2 — a publisher invitation: the
                        // co-publisher link with its kind-2 card.
                        mintCoPublisherLink(state, channelId, context)
                    } else if (public) {
                        publicLink ?: me.lakko.app.channels.ChannelInviteLinks.mintFull(context, state, client, channel, true)
                            .also { publicLink = it }
                    } else {
                        // A private channel: a one-use invitation per person.
                        me.lakko.app.channels.ChannelInviteLinks.mintFull(context, state, client, channel, false, oneUse = true)
                    }
                } catch (e: kotlinx.coroutines.CancellationException) {
                    throw e
                } catch (e: Throwable) {
                    Log.w(INFO_TAG, "invitation link not minted", e)
                    null
                }
            },
            onClose = { inviting = null },
            publisher = picking,
        )
    }
    if (confirmDelete) {
        LakkoDialog(
            onDismissRequest = { confirmDelete = false },
            title = t("channels.delete.title"),
            body = handle?.let { t("channels.delete.body", mapOf("name" to it.removePrefix("@"))) }
                ?: t("channels.delete.body_no_handle"),
            confirm = LakkoDialogAction(t("channels.delete.control"), LakkoDialogTone.Danger) {
                confirmDelete = false
                deleteError = null
                deleteScope.launch {
                    try {
                        val loop = publisherLoop ?: throw IllegalStateException("no publisher loop")
                        loop.deleteChannel(channelId)
                        onLeft()
                    } catch (e: kotlinx.coroutines.CancellationException) {
                        throw e
                    } catch (e: Throwable) {
                        Log.w(INFO_TAG, "channel delete failed: ${e.javaClass.simpleName}")
                        deleteError = UiText(me.lakko.app.channels.channelLinkErrorKey(e, "channels.delete.failed"))
                    }
                }
            },
        )
    }
    if (confirmUnsubscribe) {
        LakkoDialog(
            onDismissRequest = { confirmUnsubscribe = false },
            title = t("channels.unsubscribe.title_named", mapOf("name" to title)),
            body = t("channels.unsubscribe.body"),
            confirm = LakkoDialogAction(t("channels.unsubscribe.action"), LakkoDialogTone.Danger) {
                confirmUnsubscribe = false
                me.lakko.app.channels.ChannelUnsubscribe.leave(state, channelId, client.baseUrl)
                onLeft()
            },
        )
    }
}

/** Sections 14.1 and 15.2 — the account keys (lower-case hex) known to be
 *  in the channel: the publishers (the owner and this account among them)
 *  and subscribers known by their card. Pure. */
internal fun channelInviteExclusions(publisherKeys: List<ByteArray>, cardKeysHex: Set<String>): Set<String> =
    publisherKeys.map { reachHex(it).lowercase() }.toSet() + cardKeysHex

@Composable
private fun InfoRow(label: String, trailing: String? = null, onClick: () -> Unit) {
    HorizontalDivider()
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(vertical = 14.dp),
    ) {
        Text(label, style = MaterialTheme.typography.bodyLarge, modifier = Modifier.weight(1f))
        trailing?.let {
            Text(it, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

/**
 * The header: the 96 dp avatar, the name, the `@name` with Copy, the line and
 * the description. The owner edits the name and description in place
 * (section 12.1): Save posts a new record and republishes a public record.
 */
@Composable
private fun InfoHeader(
    channel: ChannelHandle,
    title: String,
    handle: String?,
    line: String?,
    avatar: me.lakko.app.profile.AvatarRefs?,
    canRename: Boolean,
    state: ChannelsUiState,
    client: ChannelsClient,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var editing by remember { mutableStateOf(false) }
    var name by remember(channel.id) { mutableStateOf(title) }
    var description by remember(channel.id) { mutableStateOf(channelDescription(channel).orEmpty()) }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<UiText?>(null) }
    val owner = state.keysByChannel.value.containsKey(channel.id) &&
        state.accountRoleOf(channel.id) == me.lakko.app.channels.ChannelAccountRole.OWNER
    Column(horizontalAlignment = Alignment.CenterHorizontally, modifier = Modifier.fillMaxWidth()) {
        // Section 4.5.2 — the owner sets, changes and removes the photo.
        ChannelPhotoHeader(
            title = title,
            seed = channel.id,
            avatar = avatar,
            canSet = owner,
            onUse = { encoded ->
                val refs = uploadChannelPhoto(context, encoded)
                postChannelPhoto(context, state, client, channel, refs)
            },
            onRemove = { postChannelPhoto(context, state, client, channel, null) },
        )
        Spacer(Modifier.height(12.dp))
        if (editing) {
            OutlinedTextField(
                value = name,
                onValueChange = { name = it },
                label = { Text(t("channels.new.name_label")) },
                singleLine = true,
                enabled = !busy,
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = description,
                onValueChange = { description = it },
                label = { Text(t("channels.new.description_label")) },
                enabled = !busy,
                modifier = Modifier.fillMaxWidth(),
            )
            error?.let { Text(it.text(), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                TextButton(onClick = { editing = false }, enabled = !busy) { Text(t("common.cancel")) }
                Button(
                    enabled = !busy && name.isNotBlank(),
                    onClick = {
                        busy = true
                        error = null
                        scope.launch {
                            try {
                                ChannelMetaPublisher.post(
                                    state = state,
                                    client = client,
                                    channelId = channel.id,
                                    name = name.trim(),
                                    description = description.trim(),
                                    updatedAtMs = System.currentTimeMillis().toULong(),
                                )
                                // 4.5.4 — the public record follows.
                                state.boundAccount?.let { acct ->
                                    me.lakko.app.chat.GroupSettingsStore.saveOwedChannelRecord(context, acct, channel.id, true)
                                }
                                editing = false
                            } catch (e: kotlinx.coroutines.CancellationException) {
                                throw e
                            } catch (e: Throwable) {
                                Log.w(INFO_TAG, "rename failed", e)
                                error = UiText("channels.meta.error.save")
                            } finally {
                                busy = false
                            }
                        }
                    },
                ) { Text(if (busy) t("common.saving") else t("common.save")) }
            }
        } else {
            Text(
                title,
                style = MaterialTheme.typography.headlineSmall,
                textAlign = TextAlign.Center,
                modifier = if (canRename) Modifier.clickable { editing = true } else Modifier,
            )
            handle?.let { h ->
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("@$h", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    IconButton(onClick = { copyToClipboard(context, "@$h") }) {
                        LkIcon(R.drawable.ic_lk_copy, contentDescription = t("common.copy"), size = LkIconSize.Row)
                    }
                }
            }
            line?.let { Text(it, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant) }
            channelDescription(channel)?.let {
                Spacer(Modifier.height(8.dp))
                Text(it, style = MaterialTheme.typography.bodyMedium, modifier = if (canRename) Modifier.clickable { editing = true } else Modifier)
            }
        }
    }
}

/**
 * Section 4.1 — Channel type (owner): `Who can subscribe`, the `@name`, and
 * Topic and Language while public. Saving asks first, then: the flag at the
 * channels service, the `@name` at discovery (update, or replace when it
 * changed), the record with the new flag and signed `@name`, the Discover
 * listing (public means listed).
 */
@Composable
private fun ChannelTypePage(
    channel: ChannelHandle,
    public: Boolean,
    handle: String,
    state: ChannelsUiState,
    client: ChannelsClient,
    onDone: () -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var wantPublic by remember { mutableStateOf(public) }
    var name by remember { mutableStateOf(handle) }
    var topic by remember { mutableStateOf(channel.sharedTopic) }
    var language by remember { mutableStateOf(channel.sharedLanguage) }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<UiText?>(null) }
    var confirm by remember { mutableStateOf(false) }
    val canonical = me.lakko.app.chat.canonicalHandleInput(name)
    val wellFormed = me.lakko.app.chat.handleWellFormed(canonical)

    Text(t("channel_type.who_can_subscribe"), style = MaterialTheme.typography.labelLarge)
    me.lakko.app.ui.components.LakkoPicker(
        selected = wantPublic,
        options = listOf(false to t("group_type.private"), true to t("group_type.public")),
        onPick = { wantPublic = it },
        enabled = !busy,
        heading = t("channel_type.who_can_subscribe"),
    )
    Text(
        if (wantPublic) t("channel_type.public_desc") else t("channel_type.private_desc"),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    OutlinedTextField(
        value = if (name.isEmpty()) "" else "@$name",
        onValueChange = { name = it.removePrefix("@") },
        label = { Text(t("group_type.handle")) },
        singleLine = true,
        enabled = !busy,
        isError = canonical.isNotEmpty() && !wellFormed,
        supportingText = {
            Text(
                when {
                    canonical.isNotEmpty() && !wellFormed -> t("group_handle.error.bad_form")
                    wantPublic -> t("channel_type.handle_public")
                    else -> t("channel_type.handle_private")
                },
            )
        },
        modifier = Modifier.fillMaxWidth(),
    )
    if (wantPublic) {
        ChannelTopicLanguage(topic, language, !busy, { topic = it }, { language = it })
    }
    error?.let { Text(it.text(), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
    Button(
        enabled = !busy && wellFormed,
        onClick = { confirm = true },
        modifier = Modifier.fillMaxWidth(),
    ) { Text(if (busy) t("common.saving") else t("common.save")) }

    if (confirm) {
        val renamed = canonical != handle
        val (titleKey, bodyKey, confirmKey) = when {
            wantPublic && !public -> Triple("channel_type.confirm.public.title", "channel_type.confirm.public.body", "group_type.confirm.public.confirm")
            !wantPublic && public -> Triple("channel_type.confirm.private.title", "channel_type.confirm.private.body", "group_type.confirm.private.confirm")
            renamed -> Triple("group_type.confirm.rename.title", "channel_type.confirm.rename.body", "group_type.confirm.rename.confirm")
            else -> Triple("channel_type.title", "", "common.save")
        }
        LakkoDialog(
            onDismissRequest = { confirm = false },
            title = t(titleKey),
            body = bodyKey.takeIf { it.isNotEmpty() }?.let { t(it) },
            confirm = LakkoDialogAction(t(confirmKey)) {
                confirm = false
                busy = true
                error = null
                scope.launch {
                    try {
                        saveChannelType(context, state, client, channel, public, wantPublic, handle, canonical, topic, language)
                        onDone()
                    } catch (e: kotlinx.coroutines.CancellationException) {
                        throw e
                    } catch (e: me.lakko.app.discovery.HandleWriteError) {
                        error = if (e.code == "name-taken") UiText("group_handle.error.held") else UiText("group_handle.error.failed")
                    } catch (e: me.lakko.app.discovery.DiscoveryNetworkError) {
                        error = UiText("group_type.owed")
                    } catch (e: Throwable) {
                        Log.w(INFO_TAG, "channel type not saved", e)
                        error = UiText("group_handle.error.failed")
                    } finally {
                        busy = false
                    }
                }
            },
        )
    }
}

private suspend fun saveChannelType(
    context: android.content.Context,
    state: ChannelsUiState,
    client: ChannelsClient,
    channel: ChannelHandle,
    wasPublic: Boolean,
    public: Boolean,
    oldHandle: String,
    newHandle: String,
    topic: Int,
    language: String,
) {
    val publisher = state.publisher ?: throw IllegalStateException("channel publisher identity not available")
    val shared = channel.sharedMeta()
    val name = shared?.name ?: channel.localName
    val description = shared?.description ?: channel.localDescription
    // The flag at the channels service first: a private channel refuses a
    // subscribe without an invitation.
    publisher.withIdentity(persist = false) { ChannelsClient.ownerSigner(it, channel.id) }.use {
        client.setAccess(channel.id, it, private = !public)
    }
    // Section 4.1 — going private: "Everyone subscribed stays". The rows of
    // today are readers on this device's mark (the web type change), so the
    // let-in pass never removes them for having no ticket.
    if (wasPublic && !public) {
        me.lakko.app.channels.ChannelSubscriberRows.markCurrentRowsAsReaders(context, state, client, channel.id)
    }
    val signed = publisher.withIdentity(persist = false) { identity ->
        ChannelDiscovery.writeHandle(
            identity = identity,
            channelIdHex = channel.id,
            handle = newHandle,
            public = public,
            name = name,
            description = description,
            route = when {
                oldHandle.isEmpty() -> me.lakko.app.discovery.HandleRoute.CLAIM
                oldHandle != newHandle -> me.lakko.app.discovery.HandleRoute.REPLACE
                else -> me.lakko.app.discovery.HandleRoute.UPDATE
            },
            oldHandle = oldHandle.takeIf { it.isNotEmpty() && it != newHandle },
            avatar = shared?.fullRecord()?.avatar,
        )
    }
    ChannelMetaPublisher.post(
        state = state,
        client = client,
        channelId = channel.id,
        name = name,
        description = description,
        updatedAtMs = System.currentTimeMillis().toULong(),
        topic = topic,
        language = language,
        listed = public,
        handle = ChannelDiscovery.conversationHandleOf(signed),
    )
    // ADR-0087 section 16 — public means listed; private is never listed.
    try {
        if (public) {
            me.lakko.app.discover.DiscoverListing.listChannel(state, client, publisher, channel.id, topic, language)
        } else if (channel.sharedListed) {
            me.lakko.app.discover.DiscoverListing.unlistChannel(state, client, publisher, channel.id)
        }
    } catch (e: kotlinx.coroutines.CancellationException) {
        throw e
    } catch (e: Throwable) {
        Log.w(INFO_TAG, "Discover listing not updated", e)
    }
}

/**
 * Section 15.2 — Publishers: the owner, and for the owner the people who
 * asked to publish, with Approve and Reject. The list of current publishers
 * needs a member list on the PublisherGroup FFI, which the core does not
 * have yet, so they are not listed and `Remove publisher` is not offered.
 */
@Composable
private fun PublishersPage(
    channelId: String,
    owner: Boolean,
    state: ChannelsUiState,
    onAddPublisher: (() -> Unit)? = null,
    channelName: String = "",
    nameOf: (ByteArray) -> String = { k -> k.take(4).joinToString("") { "%02x".format(it) } },
    onSteppedDown: () -> Unit = {},
) {
    val scope = rememberCoroutineScope()
    val loop = remember(state) { ChannelPublisherLoop.peek(state) }
    val pending by (loop?.pending ?: kotlinx.coroutines.flow.MutableStateFlow(emptyMap())).collectAsState()
    val sets by (loop?.publishers ?: kotlinx.coroutines.flow.MutableStateFlow(emptyMap())).collectAsState()
    var error by remember { mutableStateOf<UiText?>(null) }
    var busy by remember { mutableStateOf(false) }
    var removing by remember { mutableStateOf<ChannelPublisherLoop.Publisher?>(null) }
    var confirmStepDown by remember { mutableStateOf(false) }
    // Section 15 (phase B) — every publisher, the owner first; the owner
    // removes a publisher, a publisher steps down.
    val list = sets[channelId]
    if (list == null) {
        Text(
            if (owner) t("channels.publishers.role_owner") else t("channels.publishers.role_publisher"),
            style = MaterialTheme.typography.bodyLarge,
        )
    } else {
        for (p in list.sortedByDescending { it.owner }) {
            Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth().padding(vertical = 6.dp)) {
                Column(Modifier.weight(1f)) {
                    Text(
                        if (p.self) t("chat.author.you") else nameOf(p.accountKey),
                        style = MaterialTheme.typography.bodyLarge,
                    )
                    Text(
                        if (p.owner) t("channels.publishers.role_owner") else t("channels.publishers.role_publisher"),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                if (owner && !p.owner && !p.self) {
                    TextButton(enabled = !busy, onClick = { removing = p }) {
                        Text(t("channels.publishers.remove"), color = MaterialTheme.colorScheme.error)
                    }
                }
                if (!owner && p.self) {
                    TextButton(enabled = !busy, onClick = { confirmStepDown = true }) {
                        Text(t("channels.publishers.step_down"), color = MaterialTheme.colorScheme.error)
                    }
                }
            }
        }
    }
    removing?.let { p ->
        LakkoDialog(
            onDismissRequest = { removing = null },
            title = t("channels.publishers.remove"),
            body = t("channels.publishers.remove_body", mapOf("name" to nameOf(p.accountKey))),
            confirm = LakkoDialogAction(t("channels.publishers.remove"), LakkoDialogTone.Danger) {
                removing = null
                busy = true
                error = null
                scope.launch {
                    try {
                        loop?.removePublisher(channelId, p.accountKey)
                    } catch (e: kotlinx.coroutines.CancellationException) {
                        throw e
                    } catch (e: Throwable) {
                        Log.w(INFO_TAG, "publisher removal failed: ${e.javaClass.simpleName}")
                        error = UiText(me.lakko.app.channels.channelLinkErrorKey(e, "channels.publishers.remove_failed"))
                    } finally {
                        busy = false
                    }
                }
            },
        )
    }
    if (confirmStepDown) {
        LakkoDialog(
            onDismissRequest = { confirmStepDown = false },
            title = t("channels.publishers.step_down"),
            body = t("channels.publishers.step_down_body"),
            confirm = LakkoDialogAction(t("channels.publishers.step_down"), LakkoDialogTone.Danger) {
                confirmStepDown = false
                busy = true
                error = null
                scope.launch {
                    try {
                        loop?.stepDown(channelId)
                        onSteppedDown()
                    } catch (e: kotlinx.coroutines.CancellationException) {
                        throw e
                    } catch (e: Throwable) {
                        Log.w(INFO_TAG, "step down failed: ${e.javaClass.simpleName}")
                        error = UiText(me.lakko.app.channels.channelLinkErrorKey(e, "channels.publishers.step_down_failed"))
                    } finally {
                        busy = false
                    }
                }
            },
        )
    }
    val waiting = pending[channelId].orEmpty()
    if (owner && waiting.isNotEmpty()) {
        HorizontalDivider()
        Text(
            LocalStrings.current.format("channels.publishers.requests", emptyMap(), mapOf("count" to waiting.size.toULong())),
            style = MaterialTheme.typography.labelLarge,
        )
        for (ref in waiting) {
            Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
                Text(t("channels.copublishers.request_row"), modifier = Modifier.weight(1f))
                TextButton(
                    enabled = !busy,
                    onClick = {
                        scope.launch {
                            try {
                                loop?.reject(channelId, ref)
                            } catch (e: kotlinx.coroutines.CancellationException) {
                                throw e
                            } catch (e: Throwable) {
                                error = UiText("channels.copublishers.error.reject")
                            }
                        }
                    },
                ) { Text(t("chat.members.join.reject"), color = MaterialTheme.colorScheme.error) }
                TextButton(
                    enabled = !busy,
                    onClick = {
                        busy = true
                        scope.launch {
                            try {
                                loop?.approve(channelId, ref)
                            } catch (e: kotlinx.coroutines.CancellationException) {
                                throw e
                            } catch (e: Throwable) {
                                Log.w(INFO_TAG, "approve failed", e)
                                error = UiText("channels.copublishers.error.approve")
                            } finally {
                                busy = false
                            }
                        }
                    },
                ) { Text(t("chat.members.join.approve")) }
            }
        }
    }
    if (owner) {
        HorizontalDivider()
        // Section 15.2 — `Add publisher` picks one contact and sends a one-use
        // link in the one-to-one; there is no shareable publisher link.
        if (onAddPublisher != null) {
            OutlinedButton(onClick = onAddPublisher) { Text(t("channels.publishers.add")) }
        }
    }
    error?.let { Text(it.text(), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
}

/**
 * Section 14.2 — the channel's invite link, opening the page of section 7.2.
 * A public channel's link carries its read key; a private channel's carries a
 * fresh invite key (registered at the channels service with no use limit) and
 * the channel's admission key.
 */
@Composable
private fun InviteLinkPage(channel: ChannelHandle, public: Boolean, state: ChannelsUiState, client: ChannelsClient) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<UiText?>(null) }
    var notice by remember { mutableStateOf<UiText?>(null) }
    // Section 14.2 — the group's screen: an optional label (a subscriber who
    // came by the link reads `From link "<label>"`, section 15.1), an expiry
    // and a use limit, then the channel's links with Copy and Revoke.
    var label by remember { mutableStateOf("") }
    var expiry by remember { mutableStateOf(me.lakko.app.channels.ChannelLinkTerms.Expiry.NEVER) }
    var uses by remember { mutableStateOf(me.lakko.app.channels.ChannelLinkTerms.Uses.UNLIMITED) }
    var revoking by remember { mutableStateOf<uniffi.lakko_ffi_uniffi.LinkRecord?>(null) }
    var usedByKey by remember(channel.id) { mutableStateOf<Map<String, Long>>(emptyMap()) }
    val registry = me.lakko.app.devices.sync.DeviceGroupSyncRepository.active
    val records by (registry?.linkRecords ?: kotlinx.coroutines.flow.MutableStateFlow(emptyList())).collectAsState()
    val links = remember(records, channel.id) { me.lakko.app.channels.ChannelLinkTerms.linksOf(records, channel.id) }
    LaunchedEffect(channel.id, links.size) {
        val pub = state.publisher ?: return@LaunchedEffect
        usedByKey = try {
            me.lakko.app.channels.ChannelPublisherSigners.derive(pub, channel.id).use { client.listInviteKeyUses(channel.id, it) }
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (_: Throwable) {
            usedByKey
        }
    }
    OutlinedTextField(
        value = label,
        onValueChange = { label = me.lakko.app.ui.contacts.clampInviteLabel(it) },
        label = { Text(t("invite.links.label_field")) },
        singleLine = true,
        enabled = !busy,
        modifier = Modifier.fillMaxWidth(),
    )
    Text(t("invite.links.expires_field"), style = MaterialTheme.typography.labelLarge)
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxWidth()) {
        for (choice in me.lakko.app.channels.ChannelLinkTerms.Expiry.entries) {
            androidx.compose.material3.FilterChip(
                selected = expiry == choice,
                enabled = !busy,
                onClick = { expiry = choice },
                label = {
                    Text(
                        if (choice == me.lakko.app.channels.ChannelLinkTerms.Expiry.NEVER) {
                            t("invite.links.expires_option_never")
                        } else {
                            t("invite.links.expires_option_days", emptyMap(), mapOf("count" to choice.days.toULong()))
                        },
                    )
                },
            )
        }
    }
    Text(t("invite.links.uses_field"), style = MaterialTheme.typography.labelLarge)
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxWidth()) {
        for (choice in me.lakko.app.channels.ChannelLinkTerms.Uses.entries) {
            androidx.compose.material3.FilterChip(
                selected = uses == choice,
                enabled = !busy,
                onClick = { uses = choice },
                label = {
                    Text(
                        if (choice == me.lakko.app.channels.ChannelLinkTerms.Uses.ONE) {
                            t("invite.links.uses_option_one")
                        } else {
                            t("invite.links.uses_option_unlimited")
                        },
                    )
                },
            )
        }
    }
    Button(
        enabled = !busy,
        onClick = {
            busy = true
            error = null
            notice = null
            scope.launch {
                try {
                    val url = me.lakko.app.channels.ChannelInviteLinks.mint(
                        context, state, client, channel, public,
                        label = label.trim(),
                        expiryMs = me.lakko.app.channels.ChannelLinkTerms.expiryMs(expiry, System.currentTimeMillis()),
                        maxUses = me.lakko.app.channels.ChannelLinkTerms.maxUses(uses),
                    )
                    label = ""
                    copyToClipboard(context, url)
                    notice = UiText("chat.invite_link.copied")
                } catch (e: kotlinx.coroutines.CancellationException) {
                    throw e
                } catch (e: Throwable) {
                    Log.w(INFO_TAG, "channel invite link failed", e)
                    error = UiText(me.lakko.app.channels.channelLinkErrorKey(e))
                } finally {
                    busy = false
                }
            }
        },
        modifier = Modifier.fillMaxWidth(),
    ) { Text(t("chat.invite_link.create")) }
    notice?.let { Text(it.text(), color = MaterialTheme.colorScheme.primary, style = MaterialTheme.typography.bodySmall) }
    error?.let { Text(it.text(), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }

    HorizontalDivider()
    Text(t("invite.links.list"), style = MaterialTheme.typography.titleMedium)
    if (links.isEmpty()) {
        Text(t("invite.links.empty"), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
    for (record in links) {
        val inviteKey = remember(record.linkId) { inviteKeyHexOf(record) }
        val terms = me.lakko.app.channels.ChannelLinkTerms.termsOf(
            expiryMs = record.expiryMs.toLong(),
            maxUses = record.maxUses.toLong(),
            used = inviteKey?.let { usedByKey[it] } ?: record.uses.toLong(),
            nowMs = System.currentTimeMillis(),
        )
        Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp)) {
            Column(modifier = Modifier.weight(1f)) {
                Text(record.label.ifBlank { t("chat.invite_link.label") }, style = MaterialTheme.typography.bodyLarge)
                Text(
                    terms.map { it.text() }.joinToString(" · "),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            TextButton(onClick = {
                copyToClipboard(
                    context,
                    me.lakko.app.discovery.composeSealedInviteUrl(
                        origin = me.lakko.app.channels.CoPublisher.DEFAULT_ORIGIN,
                        prefix = me.lakko.app.channels.CoPublisher.URL_PREFIX,
                        blobId = record.ledgerId,
                        linkSecret = record.linkSecret,
                    ),
                )
                notice = UiText("chat.invite_link.copied")
            }) { Text(t("chat.invite_link.copy")) }
            TextButton(enabled = !busy, onClick = { revoking = record }) {
                Text(t("chat.invite_link.revoke"), color = MaterialTheme.colorScheme.error)
            }
        }
    }
    revoking?.let { record ->
        LakkoDialog(
            onDismissRequest = { revoking = null },
            title = t("invite.links.delete_confirm_title"),
            body = t("invite.links.delete_confirm_body"),
            confirm = LakkoDialogAction(t("chat.invite_link.revoke"), LakkoDialogTone.Danger) {
                revoking = null
                busy = true
                error = null
                scope.launch {
                    try {
                        me.lakko.app.channels.GroupLinkRegister.revoke(record, state, client)
                        notice = UiText("chat.invite_link.revoked")
                    } catch (e: kotlinx.coroutines.CancellationException) {
                        throw e
                    } catch (e: Throwable) {
                        Log.w(INFO_TAG, "channel invite link not revoked", e)
                        error = UiText(me.lakko.app.channels.channelLinkErrorKey(e, "channels.links.revoke_failed"))
                    } finally {
                        busy = false
                    }
                }
            },
        )
    }
}

/** The invite key (lower-case hex) a private link's seed names, or `null`. */
private fun inviteKeyHexOf(record: uniffi.lakko_ffi_uniffi.LinkRecord): String? {
    if (record.inviteSeed.size != 32) return null
    return try {
        uniffi.lakko_ffi_uniffi.ChannelInviteSigner.fromInviteSeed(record.inviteSeed).use { signer ->
            signer.verifyingKey().joinToString("") { "%02x".format(it) }
        }
    } catch (e: kotlinx.coroutines.CancellationException) {
        throw e
    } catch (_: Throwable) {
        null
    }
}


/** Section 15.2 — mint and upload a co-publisher link for [channelId]. */
private suspend fun mintCoPublisherLink(
    state: ChannelsUiState,
    channelId: String,
    context: android.content.Context,
): me.lakko.app.channels.ChannelLinkMint {
    val pub = state.publisher ?: throw IllegalStateException("no publisher identity")
    val handle = state.channels.value.firstOrNull { it.id == channelId }
    val name = handle?.let { it.sharedName.ifEmpty { it.localName } }
    // Phase B — the invitation names who invites, and the channel's photo.
    val inviter = state.boundAccount?.let { acct ->
        try {
            val profile = me.lakko.app.profile.OwnProfileStore.load(context, android.util.Base64.decode(acct, android.util.Base64.NO_WRAP))
            profile?.displayName?.takeIf { it.isNotBlank() } ?: profile?.username?.let { "@$it" }
        } catch (_: Throwable) {
            null
        }
    }
    val avatar = handle?.sharedMeta()?.fullRecord()?.avatar
    // ADR-0090 — the capability is signed with the owner ACCOUNT seed (its
    // issuer is the owner's account key every joiner checks the Welcome
    // against); the group is this device's leaf's.
    val mint = pub.withChannel { ids ->
        me.lakko.app.channels.CoPublisher.openPublisherGroup(ids, channelId, state.coPublished).use { pg ->
            me.lakko.app.channels.CoPublisher.mintCoPublisherLink(ids.account, pg, name, inviter, avatar)
        }
    }
    val discovery = me.lakko.app.discovery.DiscoveryClient()
    discovery.setSessionToken(me.lakko.app.auth.SessionTokens.provider?.current())
    var ledger: ByteArray? = null
    val url = discovery.use {
        me.lakko.app.channels.CoPublisher.uploadCoPublisherLink(
            mint.sealed,
            mint.linkSecret,
            it,
            state.boundAccount,
            // One use: the person named is the only one meant to ask.
            maxUses = 0L,
            register = me.lakko.app.channels.LinkRegisterEntry(
                kind = uniffi.lakko_ffi_uniffi.LinkRecordKind.PUBLISHER,
                targetId = me.lakko.app.channels.ChannelsComposer.channelIdFromHex(channelId),
                label = name.orEmpty(),
            ),
            onUploaded = { ledger = it },
        )
    }
    return me.lakko.app.channels.ChannelLinkMint(
        url,
        me.lakko.app.channels.ChannelLinkMint.cardOf(ledger, mint.linkSecret, mint.sealed),
    )
}

/** Section 18.2 — the record with a new photo (or none), and the public
 *  record republished with it. */
private suspend fun postChannelPhoto(
    context: android.content.Context,
    state: ChannelsUiState,
    client: ChannelsClient,
    channel: ChannelHandle,
    refs: me.lakko.app.profile.AvatarRefs?,
) {
    val shared = channel.sharedMeta()
    ChannelMetaPublisher.post(
        state = state,
        client = client,
        channelId = channel.id,
        name = shared?.name ?: channel.localName,
        description = shared?.description ?: channel.localDescription,
        updatedAtMs = System.currentTimeMillis().toULong(),
        avatar = refs?.toFfi(),
        clearAvatar = refs == null,
    )
    state.boundAccount?.let { acct ->
        me.lakko.app.chat.GroupSettingsStore.saveOwedChannelRecord(context, acct, channel.id, true)
    }
}

/**
 * Section 15.1 — a channel's subscribers: one row per person who shares a
 * name, then "{n} others.". In a private channel the owner and publishers
 * ([canRemove]) also see the others, one row each, and `Remove` on every row,
 * named rows included (section 7.3: the key goes and a rotation follows).
 * Built by the shared rule ([me.lakko.app.channels.ChannelSubscriberListView]),
 * so a person shown by name is never listed again among the others. Nothing
 * about seed delivery is shown.
 */
@Composable
private fun SubscribersPage(
    channelId: String,
    state: ChannelsUiState,
    client: ChannelsClient,
    count: Long?,
    canRemove: Boolean,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var members by remember(channelId) { mutableStateOf<List<me.lakko.app.channels.ChannelMemberRow>?>(null) }
    var rows by remember(channelId) { mutableStateOf<List<me.lakko.app.channels.ChannelSubscriberRows.Row>>(emptyList()) }
    var labels by remember(channelId) { mutableStateOf<Map<String, String>>(emptyMap()) }
    var failed by remember(channelId) { mutableStateOf(false) }
    var attempt by remember(channelId) { mutableStateOf(0) }
    var removing by remember { mutableStateOf<List<String>?>(null) }
    var error by remember { mutableStateOf<UiText?>(null) }
    LaunchedEffect(channelId, attempt) {
        failed = false
        try {
            val reach = ReachManager.get(context)
            members = loadChannelMembers(
                channelIdHex = channelId,
                seedForEpoch = { epoch -> me.lakko.app.channels.ChannelMemberCards.seedForEpoch(state, channelId, epoch) },
                ownSubscriberPkHex = state.subscriberKeysByChannel.value[channelId]?.publicKey?.let(::reachHex),
                ports = me.lakko.app.channels.ChannelMemberCards.ports(reach, client),
            ).rows
            if (canRemove) {
                rows = me.lakko.app.channels.ChannelSubscriberRows.read(context, state, client, channelId)
                labels = me.lakko.app.channels.ChannelSubscriberRows.linkLabels(channelId)
            }
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Throwable) {
            Log.w(INFO_TAG, "subscribers not read", e)
            failed = true
        }
    }
    val loaded = members
    when {
        failed -> {
            Text(t("channels.subscribers.load_failed"), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodyMedium)
            TextButton(onClick = { attempt += 1 }) { Text(t("common.try_again")) }
        }
        loaded == null -> androidx.compose.material3.LinearProgressIndicator(Modifier.fillMaxWidth())
        else -> {
            val list = remember(loaded, rows, count, canRemove) {
                me.lakko.app.channels.ChannelSubscriberListView.build(
                    loaded,
                    rows,
                    count,
                    canRemove,
                    state.subscriberKeysByChannel.value[channelId]?.publicKey?.let(::reachHex),
                )
            }
            if (list.people.isEmpty() && list.others.isEmpty() && (count ?: 0L) == 0L) {
                Text(t("channels.subscribers.none"), style = MaterialTheme.typography.bodyMedium)
            }
            for (person in list.people) {
                Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp)) {
                    LakkoAvatar(name = person.row.name, seed = person.row.key, size = 40.dp, avatar = person.row.avatar)
                    androidx.compose.foundation.layout.Spacer(Modifier.padding(start = 12.dp))
                    Column(modifier = Modifier.weight(1f)) {
                        Text(if (person.row.isSelf) t("channels.members.you") else person.row.name, style = MaterialTheme.typography.bodyLarge)
                        person.row.username?.takeIf { !person.row.name.startsWith("@") }?.let {
                            Text("@$it", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                    if (person.removable) {
                        TextButton(onClick = { removing = person.pubkeysHex }) {
                            Text(t("common.remove"), color = MaterialTheme.colorScheme.error)
                        }
                    }
                }
            }
            if (list.othersCount > 0L) {
                Text(
                    t("channels.subscribers.others", emptyMap(), mapOf("count" to list.othersCount.toULong())),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            for (row in list.others) {
                HorizontalDivider()
                Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp)) {
                    val label = me.lakko.app.channels.ChannelSubscriberRows.linkLabelOf(row.inviteKeyHex, labels)
                    Text(
                        if (label != null) {
                            t("channels.subscribers.from_link", mapOf("label" to label))
                        } else {
                            t("channels.subscribers.from_invitation")
                        },
                        modifier = Modifier.weight(1f),
                    )
                    TextButton(onClick = { removing = listOf(row.pubkeyHex) }) {
                        Text(t("common.remove"), color = MaterialTheme.colorScheme.error)
                    }
                }
            }
        }
    }
    error?.let { Text(it.text(), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
    removing?.let { keys ->
        LakkoDialog(
            onDismissRequest = { removing = null },
            title = t("channels.subscribers.remove_title"),
            body = t("channels.subscribers.remove_body"),
            confirm = LakkoDialogAction(t("common.remove"), LakkoDialogTone.Danger) {
                removing = null
                scope.launch {
                    // Every key of the person (one per device), each with the
                    // rotation the removal asks for.
                    val gone = HashSet<String>()
                    try {
                        for (key in keys) {
                            me.lakko.app.channels.ChannelSubscriberRows.remove(state, client, channelId, key)
                            gone += key.lowercase()
                        }
                    } catch (e: kotlinx.coroutines.CancellationException) {
                        throw e
                    } catch (e: Throwable) {
                        Log.w(INFO_TAG, "subscriber not removed", e)
                        error = UiText("group_handle.error.failed")
                    } finally {
                        if (gone.isNotEmpty()) {
                            rows = rows.filterNot { it.pubkeyHex.lowercase() in gone }
                            members = members?.mapNotNull { m ->
                                val kept = m.subscriberPksHex.filterNot { it.lowercase() in gone }
                                if (kept.isEmpty() && !m.isSelf) null else m.copy(subscriberPksHex = kept)
                            }
                        }
                    }
                }
            },
        )
    }
}
