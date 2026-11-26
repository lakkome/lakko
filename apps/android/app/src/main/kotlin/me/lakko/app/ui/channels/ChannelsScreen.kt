// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import android.text.format.DateFormat
import android.util.Log
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import me.lakko.app.channels.ChannelDiscovery
import me.lakko.app.channels.ChannelHandle
import me.lakko.app.channels.ChannelMetaPublisher
import me.lakko.app.channels.ClassifiedPost
import me.lakko.app.channels.classifyChannelPlaintext
import me.lakko.app.channels.sharedMeta
import me.lakko.app.channels.ChannelKeyStore
import me.lakko.app.channels.ChannelPublisherIdentity
import me.lakko.app.channels.ChannelPublisherKeyStore
import me.lakko.app.channels.ChannelPublisherKeys
import me.lakko.app.channels.ChannelPublisherSigners
import me.lakko.app.channels.ChannelsClient
import me.lakko.app.channels.ChannelsComposer
import me.lakko.app.channels.PendingPublisherRequests
import kotlinx.coroutines.sync.withLock
import me.lakko.app.channels.ChannelsComposerException
import me.lakko.app.channels.ChannelsUiState
import me.lakko.app.channels.CoPublishedJournalMarks
import me.lakko.app.channels.CoPublishedJournals
import me.lakko.app.channels.CoPublisher
import me.lakko.app.channels.LegacyChannelException
import me.lakko.app.channels.LegacyChannelV1
import me.lakko.app.channels.PostView
import me.lakko.app.channels.RotationCron
import me.lakko.app.channels.SharedPreferencesChannelLogStore
import me.lakko.app.channels.StoredRotationState
import me.lakko.app.channels.SubscriberAdmission
import me.lakko.app.discovery.DiscoveryHttpError
import me.lakko.app.ds.DsClient
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.ui.components.LakkoAvatar
import me.lakko.app.ui.contacts.copyToClipboard
import me.lakko.app.profile.toFfi
import uniffi.lakko_ffi_uniffi.ChannelInfoCapabilities
import uniffi.lakko_ffi_uniffi.ChannelRotationState
import uniffi.lakko_ffi_uniffi.InfoSection
import uniffi.lakko_ffi_uniffi.PublisherGroup
import uniffi.lakko_ffi_uniffi.channelInfoSections
import uniffi.lakko_ffi_uniffi.channelRotationPolicyDefault
import uniffi.lakko_ffi_uniffi.channelRotationStateAfterRotation
import java.time.ZoneId
import java.util.Locale
import me.lakko.app.ui.components.LakkoDialog
import me.lakko.app.ui.components.LakkoDialogAction
import me.lakko.app.ui.components.LakkoDialogTone
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon

/**
 * The Channels screens (docs/ux/client-redesign.md section 5.4): the list,
 * the channel view, the subscribe route, the owner's Manage screen, New
 * channel and Join as co-publisher. Each is a destination of the app's
 * `NavHost` in `MainActivity` (Material back arrow, system and predictive
 * back); the shared [ChannelsUiState] and [ChannelsClient] are created once
 * by the session shell and threaded into every destination.
 *
 * Every repository call — publish, subscribe, envelope fan-out, rotation,
 * co-publisher approve with its reseal — is the audited path it was before
 * this restyle; only the chrome and the copy changed.
 */

private const val TAG = "ChannelsScreen"

/**
 * Build the process-scoped [ChannelsUiState], persisting the subscriber
 * keypairs AND the publisher signing seeds into the hardware-backed Keystore
 * (G-29) rather than plaintext `SharedPreferences`. The per-account channel
 * log (list, subscriber seeds, rotation state) rides the same encrypted
 * subscriber-key file, and the publisher MLS identity snapshots its provider
 * through [ChannelPublisherIdentity.onDevice]. The session shell binds the
 * state to the account once the identity secrets land.
 */
@Composable
public fun rememberChannelsUiState(): ChannelsUiState {
    val context = LocalContext.current
    return remember {
        val subscriberPrefs = ChannelKeyStore.encryptedPrefs(context)
        ChannelsUiState(
            prefs = subscriberPrefs,
            publisherPrefs = ChannelPublisherKeyStore.encryptedPrefs(context),
            logStore = SharedPreferencesChannelLogStore(subscriberPrefs),
            publisher = ChannelPublisherIdentity.onDevice(context),
            orphanStore = me.lakko.app.channels.OrphanedChannelsStore.onDevice(context),
            coPublished = me.lakko.app.channels.CoPublishedJournals.onDevice(context),
            commitStore = me.lakko.app.channels.PublisherCommitStore.onDevice(context),
        )
    }
}

// ============================================================================
// Channels list
// ============================================================================

// ============================================================================
// Screen chrome shared by the pushed destinations
// ============================================================================

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ChannelTopBar(
    title: String,
    subtitle: String? = null,
    onBack: () -> Unit,
    /** docs/ux/conversation-info.md section 1 — open this channel's info.
     *  `null` on the bars that title a form rather than a conversation. */
    onShowInfo: (() -> Unit)? = null,
    actions: @Composable () -> Unit = {},
) {
    TopAppBar(
        title = {
            Column(
                modifier = if (onShowInfo != null) {
                    Modifier.clickable(onClick = onShowInfo)
                } else {
                    Modifier
                },
            ) {
                Text(title, maxLines = 1, overflow = TextOverflow.Ellipsis)
                if (subtitle != null) {
                    Text(
                        subtitle,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        },
        navigationIcon = {
            IconButton(onClick = onBack) {
                LkIcon(R.drawable.ic_lk_chevron_back, contentDescription = t("common.back"))
            }
        },
        actions = { actions() },
    )
}

/** A one-sentence error line under a form or a composer. */
@Composable
private fun ErrorLine(message: UiText, modifier: Modifier = Modifier) {
    Text(
        message.text(),
        color = MaterialTheme.colorScheme.error,
        style = MaterialTheme.typography.bodySmall,
        modifier = modifier,
    )
}

// ============================================================================
// New channel (docs/ux/group-and-channel-privacy.md sections 3.2 and 3.3)
// ============================================================================

/**
 * Create a channel: its name, description, `@name` (proposed from the name
 * until edited, `suggestHandle`, one rule) and who can subscribe. When Public,
 * Topic and Language. In order: the PublisherGroup is founded and its journal
 * claimed with the owner's root; the `@name` is claimed (a held name stops
 * here, nothing created); the channel is created on the service with its
 * flag; the metadata record (v3: the signed `@name` and the flag) is the first
 * post; a public channel files its first public seed (the publisher loop).
 */
@Composable
public fun NewChannelScreen(
    state: ChannelsUiState,
    client: ChannelsClient,
    sessionToken: String,
    onBack: () -> Unit,
    onCreated: (channelId: String) -> Unit,
) {
    var name by remember { mutableStateOf("") }
    var description by remember { mutableStateOf("") }
    var private by remember { mutableStateOf(true) }
    val random = remember { me.lakko.app.chat.handleRandom() }
    var handle by remember { mutableStateOf("") }
    var handleEdited by remember { mutableStateOf(false) }
    var topic by remember { mutableStateOf(0) }
    val appLanguage = LocalStrings.current.language.substringBefore('-').lowercase()
    var language by remember { mutableStateOf(appLanguage.takeIf { it.length in 2..3 } ?: "") }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<UiText?>(null) }
    var taken by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val context = LocalContext.current

    // Section 2.1 — the proposed name follows the name until edited.
    val suggested = remember(name, private) { suggestedChannelHandle(name, private, random) }
    val shownHandle = if (handleEdited) handle else suggested
    val canonical = me.lakko.app.chat.canonicalHandleInput(shownHandle)
    val wellFormed = remember(canonical) { me.lakko.app.chat.handleWellFormed(canonical) }

    // Section 3.3 — a public name that answers a lookup reads "taken", 500 ms
    // after typing stops, from 3 characters. A private one is checked at
    // Create only.
    LaunchedEffect(canonical, private) {
        taken = false
        if (private || canonical.length < 3 || !wellFormed) return@LaunchedEffect
        // A name a failed create of this account claimed is this account's (a
        // retry takes it back): the lookup answering it is not "taken".
        // Read under the account the create keeps it under.
        val claims = me.lakko.app.channels.ChannelHandleClaims.on(context)
        if (claims.isClaimed(state.boundAccount.orEmpty(), canonical)) return@LaunchedEffect
        delay(500)
        taken = try {
            me.lakko.app.discovery.DiscoveryClient().use { it.lookupChannel(canonical) != null }
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (_: Throwable) {
            false
        }
    }

    Scaffold(topBar = { ChannelTopBar(title = t("channels.new.title"), onBack = onBack) }) { inner ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(inner)
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            // Before a name is typed: the group glyph, as on New group, never
            // a "?" from initials of nothing.
            LakkoAvatar(
                name = name,
                seed = name.ifBlank { "new-channel" },
                size = 96.dp,
                group = true,
                groupGlyph = name.isBlank(),
                modifier = Modifier.align(Alignment.CenterHorizontally),
            )
            OutlinedTextField(
                value = name,
                onValueChange = { name = it },
                label = { Text(t("channels.new.name_label")) },
                enabled = !busy,
                singleLine = true,
                keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Sentences),
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = description,
                onValueChange = { description = it },
                label = { Text(t("channels.new.description_label")) },
                enabled = !busy,
                keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Sentences),
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = if (shownHandle.isEmpty()) "" else "@$shownHandle",
                onValueChange = {
                    handleEdited = true
                    handle = it.removePrefix("@")
                },
                label = { Text(t("group_type.handle")) },
                enabled = !busy,
                singleLine = true,
                isError = canonical.isNotEmpty() && !wellFormed || taken,
                supportingText = {
                    Text(
                        when {
                            canonical.isNotEmpty() && !wellFormed -> t("group_handle.error.bad_form")
                            taken -> t("group_handle.error.taken")
                            private -> t("channel_type.handle_private")
                            else -> t("channel_type.handle_public")
                        },
                    )
                },
                modifier = Modifier.fillMaxWidth(),
            )
            Text(t("channel_type.who_can_subscribe"), style = MaterialTheme.typography.labelLarge)
            me.lakko.app.ui.components.LakkoPicker(
                selected = private,
                options = listOf(true to t("group_type.private"), false to t("group_type.public")),
                onPick = { private = it },
                enabled = !busy,
                heading = t("channel_type.who_can_subscribe"),
            )
            Text(
                if (private) t("channel_type.private_desc") else t("channel_type.public_desc"),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (!private) {
                ChannelTopicLanguage(
                    topic = topic,
                    language = language,
                    enabled = !busy,
                    onTopic = { topic = it },
                    onLanguage = { language = it },
                )
            }
            error?.let { ErrorLine(it) }
            Button(
                onClick = {
                    if (busy || name.trim().isEmpty() || !wellFormed) return@Button
                    busy = true
                    error = null
                    val wanted = canonical
                    val claims = me.lakko.app.channels.ChannelHandleClaims.on(context)
                    val claimAccount = state.boundAccount.orEmpty()
                    // A retry of a failed create reuses the id its name was
                    // claimed for, only while that create is recorded as
                    // unfinished (security pass F5): a finished create's id
                    // may be a live channel, never founded over.
                    val id = me.lakko.app.channels.newChannelCreateId(claims, claimAccount, wanted) { cid ->
                        me.lakko.app.channels.ChannelCreatePending.isPending(context, claimAccount, cid)
                    }
                    scope.launch {
                        try {
                            createChannel(
                                state = state,
                                client = client,
                                recordOwed = me.lakko.app.channels.PrefsOwedRecordMarks(context) { state.boundAccount },
                                id = id,
                                name = name.trim(),
                                description = description.trim(),
                                handle = wanted,
                                private = private,
                                topic = topic,
                                language = language,
                            )
                            claims.forget(claimAccount, wanted)
                            onCreated(id)
                        } catch (e: kotlinx.coroutines.CancellationException) {
                            throw e
                        } catch (e: LegacyChannelException) {
                            error = e.text
                        } catch (e: me.lakko.app.discovery.HandleWriteError) {
                            Log.w(TAG, "channel @name not claimed: ${e.status}")
                            if (e.code == "name-taken") claims.forget(claimAccount, wanted)
                            error = when {
                                e.code == "name-taken" -> UiText("group_handle.error.held")
                                e.status == 429 -> UiText("channels.new.too_many")
                                else -> UiText("channels.new.failed")
                            }
                        } catch (e: me.lakko.app.anon.AnonTokenQuotaExhausted) {
                            error = UiText("channels.new.too_many")
                        } catch (e: me.lakko.app.discovery.DiscoveryNetworkError) {
                            error = UiText("group_handle.error.offline")
                        } catch (e: ChannelCreateOffline) {
                            error = UiText("group_handle.error.offline")
                        } catch (e: Throwable) {
                            Log.w(TAG, "create channel failed", e)
                            error = me.lakko.app.chat.handleWriteFailure(e).takeIf {
                                it.key == "group_handle.error.offline" || it.key == "common.error.too_many_attempts"
                            } ?: UiText("channels.new.failed")
                        } finally {
                            busy = false
                        }
                    }
                },
                enabled = !busy && name.trim().isNotEmpty() && wellFormed,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(if (busy) t("common.creating") else t("channels.new.create"))
            }
        }
    }
}

/** The channel journal claim did not land: nothing was created. */
private class ChannelCreateOffline(cause: Throwable) : Exception(cause)

/** Section 2.1 — the proposed `@name` of a channel (`suggestHandle`). */
private fun suggestedChannelHandle(name: String, private: Boolean, random: ULong): String = try {
    uniffi.lakko_ffi_uniffi.suggestHandle(name, uniffi.lakko_ffi_uniffi.HandleKind.CHANNEL, private, random)
} catch (_: Throwable) {
    ""
}

/** The New channel steps, in the order of [NewChannelScreen]. */
internal suspend fun createChannel(
    state: ChannelsUiState,
    client: ChannelsClient,
    recordOwed: me.lakko.app.channels.OwedRecordMarks,
    id: String,
    name: String,
    description: String,
    handle: String,
    private: Boolean,
    topic: Int,
    language: String,
) {
    val publisher = state.publisher ?: throw IllegalStateException("channel publisher identity not available")
    // Security pass F5 (2026-11-15) - this create is unfinished until the
    // channel exists here: only then may a retry reuse [id]. Written before
    // anything is founded or claimed under it.
    val pendingContext = me.lakko.app.channels.SubscriberAdmission.appContext
        ?: throw IllegalStateException("no context for the channel create")
    val pendingAccount = state.boundAccount ?: throw IllegalStateException("no account bound")
    if (!me.lakko.app.channels.ChannelCreatePending.mark(pendingContext, pendingAccount, id)) {
        throw IllegalStateException("the channel create was not recorded")
    }
    // ADR-0057 finding 3 — the PublisherGroup journal is claimed with the
    // owner's root BEFORE anything announces the channel.
    try {
        publisher.withChannel { ids ->
            DsClient().use { ds -> CoPublisher.foundAndClaimAtCreation(ds, ids, id) }
        }
    } catch (e: LegacyChannelException) {
        throw e
    } catch (e: kotlinx.coroutines.CancellationException) {
        throw e
    } catch (e: Throwable) {
        Log.w(TAG, "channel journal claim did not land; not created", e)
        throw ChannelCreateOffline(e)
    }
    // Section 3 — the @name first: a held name stops here, nothing created.
    val signed = publisher.withIdentity(persist = false) { identity ->
        ChannelDiscovery.writeHandle(
            identity = identity,
            channelIdHex = id,
            handle = handle,
            public = !private,
            name = name,
            description = description,
            route = me.lakko.app.discovery.HandleRoute.CLAIM,
        )
    }
    // Security pass F1 (2026-11-15) - a private channel's admission seed
    // version 0 is minted here, before the service knows the channel, with
    // the admission mark at 0 set by this device: no row can be genuine yet,
    // so no row is listed or marked (a row the service lists now was planted
    // by it, and a ticketless one is removed by the let-in pass). A retry
    // finds the seed it minted.
    if (private) {
        val context = me.lakko.app.channels.SubscriberAdmission.appContext
            ?: throw IllegalStateException("no context for the channel's admission seed")
        val account = state.boundAccount ?: throw IllegalStateException("no account bound")
        me.lakko.app.channels.admissionSeedFor(
            reason = me.lakko.app.channels.AdmissionSeedReason.CREATE,
            held = { me.lakko.app.channels.ChannelAdmissionKeys.current(context, account, id) },
            mint = { me.lakko.app.channels.ChannelAdmissionKeys.currentOrMint(context, account, id) },
            markRowsAsReaders = {},
        )?.second?.fill(0)
        me.lakko.app.channels.ChannelAdmissionMark.raise(context, account, id, 0L, markSet = true)
    }
    val owner = publisher.withIdentity(persist = false) { identity -> ChannelsClient.ownerSigner(identity, id) }
    owner.use { client.createChannel(id, it, private = private) }
    val keys = publisher.withChannel { ids ->
        // New channel is the one flow that may found the PublisherGroup
        // (docs/ux/second-device.md section 4a); it was founded and claimed
        // just above.
        ChannelsComposer.freshPublisherKeys(
            ids = ids,
            channelIdHex = id,
            existingSigningSeed = state.persistedPublisherSigningSeed(id),
            allowFound = true,
        )
    }
    state.addChannel(
        ChannelHandle(
            id = id,
            localName = name,
            localDescription = description,
            addedAtMs = System.currentTimeMillis().toULong(),
            handle = handle,
        ),
    )
    state.setKeys(id, keys)
    // ADR-0090 decision A — a channel founded here is never founded again by
    // a pending re-founding of this device.
    state.boundAccount?.let { acct ->
        me.lakko.app.channels.ChannelRefounding.markDone(context = me.lakko.app.channels.SubscriberAdmission.appContext ?: return@let, account = acct, channelId = id)
    }
    // F5 - the channel exists here: its id is never reused by a retry.
    me.lakko.app.channels.ChannelCreatePending.clear(pendingContext, pendingAccount, id)
    // ADR-0051 §4 / ADR-0087 section 18.2 — the v3 record is the first post:
    // subscribers title the channel from it and read its @name and flag. It
    // is kept here before it is sent and owed until it lands, so a failed
    // post leaves the channel created and the publisher loop re-issues THIS
    // record (never one composed without the flag).
    me.lakko.app.channels.postFirstChannelRecord(recordOwed, id) {
        ChannelMetaPublisher.post(
            state = state,
            client = client,
            channelId = id,
            name = name,
            description = description,
            updatedAtMs = System.currentTimeMillis().toULong(),
            topic = topic,
            language = language,
            listed = !private,
            handle = ChannelDiscovery.conversationHandleOf(signed),
            keepBeforeSend = true,
        )
    }
    // ADR-0087 section 16 — public means listed in Discover.
    if (!private) {
        try {
            me.lakko.app.discover.DiscoverListing.listChannel(state, client, publisher, id, topic, language)
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Throwable) {
            Log.w(TAG, "Discover listing not published at creation", e)
        }
    }
}

/** The topic and language pickers of a public channel (section 3.2). */
@Composable
internal fun ChannelTopicLanguage(
    topic: Int,
    language: String,
    enabled: Boolean,
    onTopic: (Int) -> Unit,
    onLanguage: (String) -> Unit,
) {
    val topics = remember {
        try {
            uniffi.lakko_ffi_uniffi.channelTopics().map { it.id.toInt() to it.name }
        } catch (_: Throwable) {
            listOf(0 to "Other")
        }
    }
    val languages = remember {
        try {
            uniffi.lakko_ffi_uniffi.l10nSupportedLanguages().map { it.code.substringBefore('-').lowercase() to it.ownName }
        } catch (_: Throwable) {
            emptyList()
        }.distinctBy { it.first }
    }
    Text(t("discover.listing.topic"), style = MaterialTheme.typography.labelLarge)
    me.lakko.app.ui.components.LakkoPicker(
        selected = topic,
        options = topics,
        onPick = onTopic,
        enabled = enabled,
        heading = t("discover.listing.topic"),
    )
    if (languages.isNotEmpty()) {
        Text(t("discover.language"), style = MaterialTheme.typography.labelLarge)
        me.lakko.app.ui.components.LakkoPicker(
            selected = language,
            options = languages,
            onPick = onLanguage,
            enabled = enabled,
            heading = t("discover.language"),
        )
    }
}


// ============================================================================
// Join as co-publisher (ADR-0028 OQ3, joiner side)
// ============================================================================

/**
 * Paste a co-publisher link, ask to join, wait for the owner's approval, then
 * mint local publisher keys and open the channel.
 */
@Composable
public fun JoinCoPublisherScreen(
    state: ChannelsUiState,
    onBack: () -> Unit,
    onJoined: (channelId: String) -> Unit,
    /** ADR-0087 section 15.2 — the co-publisher link that opened this page. */
    initialLink: String? = null,
) {
    var link by remember { mutableStateOf(initialLink.orEmpty()) }
    var localName by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var status by remember { mutableStateOf<UiText?>(null) }
    var error by remember { mutableStateOf<UiText?>(null) }
    // ADR-0053 — "Try again" is offered only with the general failure.
    var offerRetry by remember { mutableStateOf(false) }
    val ds = remember { DsClient() }
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    // ADR-0057 — marks the channel's journal as co-published, so this device
    // never claims it.
    val coPublished = remember(context, state) { CoPublishedJournals.onDevice(context) { state.boundAccount } }

    // The stored name of a channel joined without one: a data value, taken in
    // the language on screen when the channel is added.
    val defaultName = t("channels.join.default_name")
    Scaffold(topBar = { ChannelTopBar(title = t("channels.join.title"), onBack = onBack) }) { inner ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(inner)
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                t("channels.join.subtitle"),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            OutlinedTextField(
                value = link,
                onValueChange = { link = it },
                label = { Text(t("channels.join.link_label")) },
                enabled = !busy,
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = localName,
                onValueChange = { localName = it },
                label = { Text(t("channels.join.name_label")) },
                enabled = !busy,
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            // Sends the request; reached from `Request to join` and from
            // `Try again` after a general failure.
            var waitingForOwner by remember { mutableStateOf(false) }
            fun submit() {
                val trimmed = link.trim()
                if (busy || trimmed.isEmpty()) return
                busy = true
                error = null
                offerRetry = false
                status = UiText("group.join.sending")
                scope.launch {
                    try {
                        val joined = joinAsCoPublisher(
                            context,
                            state,
                            ds,
                            trimmed,
                            localName.trim().ifEmpty { defaultName },
                            coPublished,
                        ) { status = it }
                        if (joined == null) {
                            // Not approved yet: the request stays and lands
                            // by itself (no deadline).
                            waitingForOwner = true
                            return@launch
                        }
                        onJoined(joined)
                    } catch (e: kotlinx.coroutines.CancellationException) {
                        throw e
                    } catch (e: LegacyChannelException) {
                        error = e.text
                    } catch (e: Throwable) {
                        me.lakko.app.rethrowIfCancelled(e)
                        // The raw cause goes to the log; the person reads
                        // one of the join screen's fixed sentences (the
                        // "isn't taking requests yet" one when the owner
                        // has not claimed the journal; nothing was sent).
                        Log.w(TAG, "co-publish request failed", e)
                        val copy = me.lakko.app.chat.joinFailureCopy(e)
                        error = copy
                        offerRetry = me.lakko.app.chat.joinFailureOffersRetry(copy)
                    } finally {
                        busy = false
                        status = if (waitingForOwner) UiText("channels.join.waiting") else null
                    }
                }
            }
            Button(
                onClick = { submit() },
                enabled = !busy && !waitingForOwner && link.trim().isNotEmpty(),
                modifier = Modifier.fillMaxWidth(),
            ) { Text(if (busy) t("channels.join.requesting") else t("group.join.request")) }

            status?.let { Text(it.text(), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant) }
            error?.let { ErrorLine(it) }
            if (error != null && offerRetry) {
                Button(
                    onClick = { submit() },
                    enabled = !busy,
                    modifier = Modifier.fillMaxWidth(),
                ) { Text(me.lakko.app.chat.JOIN_TRY_AGAIN_TEXT.text()) }
            }
        }
    }
}


/**
 * Ask to join a channel as a co-publisher through [link] (a `/g/` co-publisher
 * link), wait for the owner's device to approve, then mint local publisher
 * keys and add the channel. Returns the channel id, or `null` when the owner's
 * device did not approve within this wait. The REQUEST has no deadline
 * (ADR-0045 decision 2): it is recorded before the knock
 * ([PendingPublisherRequests]) and the account mailbox loop lands the
 * owner's Welcome whenever it arrives ([landPendingPublisherWelcome]), so a
 * `null` means "waiting", never "refused". [onStatus] receives the progress
 * line. Throws on a refused or failed request ([LegacyChannelException] for
 * an old link). Shared by the join page and the publisher invitation card
 * (UX section 15.2).
 */
public suspend fun joinAsCoPublisher(
    context: android.content.Context,
    state: ChannelsUiState,
    ds: DsClient,
    link: String,
    channelName: String,
    coPublished: CoPublishedJournalMarks,
    onStatus: (UiText?) -> Unit,
): String? {
    val publisher = state.publisher
        ?: throw IllegalStateException("channel publisher identity not available")
    val account = state.boundAccount ?: throw IllegalStateException("no account bound")
    onStatus(UiText("group.join.sending"))
    // The request mints a KeyPackage whose private keys live in the shared
    // identity's provider; the join below must run on the SAME identity, and
    // the joined group is persisted with it.
    val request = me.lakko.app.discovery.DiscoveryClient().use { discovery ->
        publisher.withChannel { ids ->
            CoPublisher.requestToCoPublish(
                ds,
                ids,
                link,
                coPublished,
                { blobId -> discovery.fetchGroupInvite(blobId) },
                ownProfileEnvelopeWithAccount(context, state.boundAccount),
                beforeKnock = { r ->
                    PendingPublisherRequests.record(
                        context,
                        account,
                        PendingPublisherRequests.Request(
                            channelIdHex = r.channelIdHex,
                            groupIdHex = ChannelsComposer.bytesToHex(r.groupId),
                            joinerKeyB64 = java.util.Base64.getEncoder().encodeToString(r.joinerKey),
                            name = channelName,
                            requestedAtMs = System.currentTimeMillis(),
                            ownerAccountKeyB64 = java.util.Base64.getEncoder().encodeToString(r.ownerAccountKey),
                        ),
                    )
                },
            )
        }
    }
    onStatus(UiText("channels.join.waiting"))
    var attempts = 0
    while (attempts < ACCEPT_WAIT_POLLS) {
        // Landed by the account mailbox loop meanwhile.
        if (!PendingPublisherRequests.isPending(context, account, request.channelIdHex) &&
            state.keysByChannel.value.containsKey(request.channelIdHex)
        ) {
            return request.channelIdHex
        }
        val landed = publisherLanding.withLock {
            if (!PendingPublisherRequests.isPending(context, account, request.channelIdHex)) {
                return@withLock state.keysByChannel.value.containsKey(request.channelIdHex)
            }
            val keys = publisher.withChannel { ids ->
                // Joined: the PublisherGroup is persisted with the device's
                // provider at the post-approve epoch before anything else.
                CoPublisher.pollForCoPublishWelcome(
                    ds,
                    ids,
                    request.groupId,
                    request.ownerAccountKey,
                    request.channelIdHex,
                    coPublished,
                    // G-29 — reuse the persisted signing seed on a re-poll.
                    state.persistedPublisherSigningSeed(request.channelIdHex),
                )
            } ?: return@withLock false
            settleLandedRequest(context, state, account, request.channelIdHex, channelName, keys)
            true
        }
        if (landed) return request.channelIdHex
        delay(3_000L)
        attempts += 1
    }
    return null
}

/** How many 3-second passes the Accept wait itself makes (three minutes,
 *  chosen); the request outlives it. */
private const val ACCEPT_WAIT_POLLS = 60

/** One landing at a time: the Accept wait and the mailbox loop may hold the
 *  same Welcome. */
private val publisherLanding = kotlinx.coroutines.sync.Mutex()

/** A request landed: the channel and its keys, then the record goes. */
private fun settleLandedRequest(
    context: android.content.Context,
    state: ChannelsUiState,
    account: String,
    channelIdHex: String,
    channelName: String,
    keys: ChannelPublisherKeys,
) {
    if (state.channels.value.none { it.id == channelIdHex }) {
        state.addChannel(
            ChannelHandle(
                id = channelIdHex,
                localName = channelName,
                localDescription = "",
                addedAtMs = System.currentTimeMillis().toULong(),
            ),
        )
    }
    state.setKeys(channelIdHex, keys)
    PendingPublisherRequests.drop(context, account, channelIdHex)
}

/**
 * The account mailbox loop's half of a publisher request (ADR-0045 decision
 * 2: no deadline): try one Welcome from the account mailbox (the raw Welcome a
 * `WELCOME_RENDEZVOUS` or `WELCOME` row carries) against every pending
 * request of this account. Returns whether it landed one; a Welcome that is
 * no request's changes nothing. Throws when a landed join could not be made
 * durable (the caller then holds its mailbox cursor).
 */
public suspend fun landPendingPublisherWelcome(
    context: android.content.Context,
    state: ChannelsUiState,
    coPublished: CoPublishedJournalMarks,
    welcome: ByteArray,
): Boolean {
    val account = state.boundAccount ?: return false
    val publisher = state.publisher ?: return false
    if (!publisher.hasDevice()) return false
    return publisherLanding.withLock {
        val ownKey = publisher.withChannel { ids -> java.util.Base64.getEncoder().encodeToString(ids.device.signaturePublicKey()) }
        for (r in PendingPublisherRequests.load(context, account)) {
            // The Welcome is sealed to the knocked KeyPackage, whose private
            // key is in this device's provider only when the request was made
            // on it.
            if (r.joinerKeyB64 != ownKey) continue
            val groupId = hexToBytesOrNull(r.groupIdHex) ?: continue
            val ownerKey = r.ownerAccountKeyB64?.let {
                try {
                    java.util.Base64.getDecoder().decode(it)
                } catch (_: IllegalArgumentException) {
                    null
                }
            } ?: continue
            val keys = publisher.withChannel { ids ->
                CoPublisher.joinCoPublishWelcome(
                    ids,
                    welcome,
                    groupId,
                    ownerKey,
                    r.channelIdHex,
                    coPublished,
                    state.persistedPublisherSigningSeed(r.channelIdHex),
                )
            } ?: continue
            settleLandedRequest(context, state, account, r.channelIdHex, r.name, keys)
            return@withLock true
        }
        // ADR-0090 section 2.3 — an owner device admitted this device to a
        // channel the account owns or co-publishes (the subscription
        // register). Offered to every such channel, a channel whose group or
        // keys this device still holds included (walk of 2026-11-14): a device
        // that returns after another device retired its leaf holds its old
        // group, and the replace rule ([CoPublisher.joinAdmissionWelcome]:
        // same group id, signed by a device of the owner account, the held
        // group restored if the join fails) decides.
        val registers = me.lakko.app.devices.sync.DeviceGroupSyncRepository.active?.channelSubscriptions?.value.orEmpty()
        val targets = me.lakko.app.channels.publisherWelcomeTargets(
            registers.map { me.lakko.app.channels.viewOf(it) },
            state.channels.value.map { it.id }.toSet(),
        )
        if (targets.isEmpty()) return@withLock false
        val target = publisher.withChannel { ids ->
            val welcomeGroupId = CoPublisher.welcomeGroupId(ids, welcome)
            me.lakko.app.channels.publisherWelcomeTarget(targets, welcomeGroupId) { t ->
                CoPublisher.expectedPublisherGroupId(
                    ids,
                    t.channelIdHex,
                    owned = t.role == me.lakko.app.channels.ChannelAccountRole.OWNER,
                    joinedGroupId = t.publisherGroupId,
                )
            }
        } ?: return@withLock false
        val channelIdHex = state.channels.value.firstOrNull { it.id.equals(target.channelIdHex, ignoreCase = true) }?.id
            ?: return@withLock false
        val owned = target.role == me.lakko.app.channels.ChannelAccountRole.OWNER
        val join: suspend () -> me.lakko.app.channels.ChannelPublisherKeys? = {
            try {
                publisher.withChannel { ids ->
                    CoPublisher.joinAdmissionWelcome(
                        ids,
                        welcome,
                        channelIdHex,
                        owned = owned,
                        joinedGroupId = if (owned) null else target.publisherGroupId.takeIf { it.size == 32 },
                        ownerAccountKey = if (owned) {
                            null
                        } else {
                            me.lakko.app.channels.coPublisherWelcomeOwnerKey(target.ownerAccountKey, coPublished.ownerKeyOf(channelIdHex))
                        },
                        coPublished = coPublished,
                        existingSigningSeed = state.persistedPublisherSigningSeed(channelIdHex),
                    )
                }
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (_: Throwable) {
                null
            }
        }
        // The publisher loop's open handle of a held group is closed first,
        // under its lock, so no pass acts on the replaced group's old state.
        val loop = me.lakko.app.channels.ChannelPublisherLoop.peek(state)
        val keys = (if (loop != null) loop.releasingGroup(channelIdHex) { join() } else join())
            ?: return@withLock false
        state.setKeys(channelIdHex, keys)
        Log.i(TAG, "ADR-0090: admitted to post in a channel of the account")
        true
    }
}

private fun hexToBytesOrNull(hex: String): ByteArray? {
    if (hex.isEmpty() || hex.length % 2 != 0) return null
    return try {
        ByteArray(hex.length / 2) { i -> hex.substring(i * 2, i * 2 + 2).toInt(16).toByte() }
    } catch (_: NumberFormatException) {
        null
    }
}

/** ADR-0087 section 7 — this account's profile envelope carrying its account
 *  id, for a co-publisher request; empty when none can be composed. */
private fun ownProfileEnvelopeWithAccount(context: android.content.Context, accountIdB64: String?): ByteArray {
    val acct = accountIdB64 ?: return ByteArray(0)
    val accountId = try {
        java.util.Base64.getDecoder().decode(acct)
    } catch (_: IllegalArgumentException) {
        return ByteArray(0)
    }
    return try {
        me.lakko.app.profile.OwnProfileStore.load(context, accountId)
            ?.toFfi(accountId = accountId, shareUsername = false)
            ?.let { me.lakko.app.profile.ProfileSigner.compose(it, accountId) }
            ?: ByteArray(0)
    } catch (_: Throwable) {
        ByteArray(0)
    }
}
