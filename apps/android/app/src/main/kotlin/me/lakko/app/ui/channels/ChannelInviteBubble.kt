// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.ui.components.LakkoAvatar

/** What a channel's `/g/` link in a message opens to. */
public class ChannelInviteFacts(
    public val channelIdHex: String,
    public val name: String,
    public val preview: me.lakko.app.chat.GroupInvitePreviewUi,
    /** The channel's photo the invitation carries, when it carries one. */
    public val avatar: me.lakko.app.profile.AvatarRefs? = null,
) {
    public val publisherInvite: Boolean get() = preview.coPublisher
    public val public: Boolean get() = preview.subscriber?.access is uniffi.lakko_ffi_uniffi.FfiChannelAccess.Public
}

/** What the card in a chat needs from the session (ADR-0087 section 14.1). */
public interface ChannelInviteActions {
    /** The link's facts when it is a channel's, else `null`. */
    public suspend fun inspect(link: me.lakko.app.contacts.LakkoLink.Group): ChannelInviteFacts?
    public fun isFollowed(channelIdHex: String): Boolean
    public suspend fun subscribe(facts: ChannelInviteFacts)
    public fun openChannel(channelIdHex: String)
    /** A publisher invitation's `Accept` (UX section 15.2): asks to join and
     *  waits for the owner's device, with no further step. Returns the
     *  channel id, or `null` when the owner's device has not approved within
     *  the wait. [onStatus] receives the progress line. Throws on a failure. */
    public suspend fun acceptPublisher(
        facts: ChannelInviteFacts,
        link: me.lakko.app.contacts.LakkoLink.Group,
        onStatus: (me.lakko.app.l10n.UiText?) -> Unit,
    ): String?
}

/** Set by the session shell; `null` outside a session. */
public object ChannelInviteActionsHolder {
    @Volatile
    public var actions: ChannelInviteActions? = null
}

/** Opened channel invitations, by (conversation, DS message id), for the
 *  process. */
private val inspected = HashMap<String, ChannelInviteFacts?>()

/** What a card opened to without the network: the facts to draw, and the
 *  link the card carries (what Subscribe and Accept act through). */
private class OpenedCard(val facts: ChannelInviteFacts, val link: me.lakko.app.contacts.LakkoLink.Group)

/** Open a channel invitation card offline (`openChannelInviteCard`): kind 2
 *  (a publisher invitation) and kind 3 (a subscriber invitation). */
private fun openCard(card: ByteArray): OpenedCard? {
    val opened = try {
        uniffi.lakko_ffi_uniffi.openChannelInviteCard(card)
    } catch (_: Throwable) {
        return null
    }
    val preview = opened.preview
    val channelId = preview.channelId ?: return null
    val url = try {
        me.lakko.app.discovery.composeSealedInviteUrl(
            origin = me.lakko.app.config.ApiEndpoints.webOrigin,
            prefix = "/g/",
            blobId = opened.ledgerId,
            linkSecret = opened.linkSecret,
        )
    } catch (_: Throwable) {
        return null
    }
    val link = me.lakko.app.contacts.lakkoLinkAt(url, me.lakko.app.config.ApiEndpoints.webOrigin)
        as? me.lakko.app.contacts.LakkoLink.Group ?: return null
    val ui = me.lakko.app.chat.GroupInvitePreviewUi(
        groupName = preview.groupName,
        inviterName = preview.inviterName,
        memberCount = preview.memberCount.toInt(),
        channelId = channelId,
        subscriber = preview.subscriber,
    )
    val name = preview.groupName?.takeIf { it.isNotBlank() } ?: me.lakko.app.l10n.Localization.strings.value["channels.fallback_name"]
    return OpenedCard(
        ChannelInviteFacts(
            ChannelsComposerHex.hex(channelId),
            name,
            ui,
            avatar = me.lakko.app.profile.avatarRefsOf(preview.avatar),
        ),
        link,
    )
}

private object ChannelsComposerHex {
    fun hex(b: ByteArray): String = b.joinToString("") { "%02x".format(it) }
}

/**
 * Section 14 — a channel invitation in a one-to-one, drawn as a card: the
 * channel's name, what it is, and `Subscribe` (`Open` once subscribed). A
 * publisher invitation (kind 2) reads "... invites you to be a publisher"
 * with `Accept`, which asks to join right there and shows the progress and
 * any error under the button (UX section 15.2: no further step). A message carrying a card
 * ([card]) is opened offline; an older message that carries only the link
 * text is opened through the link. Keyed by (conversation, DS message id).
 */
@Composable
internal fun ChannelInviteBubbleCard(
    text: String,
    card: ByteArray? = null,
    messageId: Long = 0L,
    /** The bubble's own colours, so the card reads on either bubble: the
     *  button is drawn in the bubble's text colour with the bubble's colour
     *  as its label (as the call invitation does). `null` outside a bubble. */
    bubbleColor: androidx.compose.ui.graphics.Color? = null,
    contentColor: androidx.compose.ui.graphics.Color? = null,
) {
    val actions = ChannelInviteActionsHolder.actions ?: return
    val conversation = me.lakko.app.ui.translation.LocalTranslationConversation.current
    val key = "${conversation.orEmpty()}:$messageId:${if (card != null) "card" else text.trim().hashCode()}"
    val fromCard = remember(key) { card?.let(::openCard) }
    val textLink = remember(key) {
        if (card != null) {
            null
        } else {
            me.lakko.app.contacts.lakkoLinkAt(text.trim(), me.lakko.app.config.ApiEndpoints.webOrigin)
                as? me.lakko.app.contacts.LakkoLink.Group
        }
    }
    if (fromCard == null && textLink == null) return
    var facts by remember(key) { mutableStateOf(fromCard?.facts ?: inspected[key]) }
    LaunchedEffect(key) {
        if (fromCard == null && textLink != null && !inspected.containsKey(key)) {
            val f = try {
                actions.inspect(textLink)
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (_: Throwable) {
                null
            }
            inspected[key] = f
            facts = f
        }
    }
    val f = facts ?: return
    val link = fromCard?.link ?: textLink ?: return
    val scope = rememberCoroutineScope()
    var busy by remember { mutableStateOf(false) }
    var acceptStatus by remember(key) { mutableStateOf<me.lakko.app.l10n.UiText?>(null) }
    var acceptError by remember(key) { mutableStateOf<me.lakko.app.l10n.UiText?>(null) }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        modifier = Modifier.padding(top = 6.dp),
    ) {
        LakkoAvatar(name = f.name, seed = f.channelIdHex, size = 40.dp, group = true, avatar = f.avatar)
        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(f.name, style = MaterialTheme.typography.titleSmall)
            Text(
                if (f.publisherInvite) {
                    t("channels.publishers.invite_page", mapOf("channel" to f.name))
                } else if (f.public) {
                    t("channels.header.public")
                } else {
                    t("channels.header.private")
                },
                style = MaterialTheme.typography.bodySmall,
            )
            when {
                f.publisherInvite && actions.isFollowed(f.channelIdHex) && acceptStatus == null ->
                    CardButton(t("common.open"), true, bubbleColor, contentColor) { actions.openChannel(f.channelIdHex) }
                f.publisherInvite -> {
                    CardButton(
                        label = if (busy) t("channels.join.requesting") else t("channels.publishers.accept"),
                        enabled = !busy,
                        bubbleColor = bubbleColor,
                        contentColor = contentColor,
                        onClick = {
                            busy = true
                            acceptError = null
                            scope.launch {
                                try {
                                    val joined = actions.acceptPublisher(f, link) { acceptStatus = it }
                                    acceptStatus = null
                                    if (joined == null) {
                                        acceptError = me.lakko.app.l10n.UiText("channels.join.no_approval")
                                    } else {
                                        actions.openChannel(joined)
                                    }
                                } catch (e: kotlinx.coroutines.CancellationException) {
                                    throw e
                                } catch (e: me.lakko.app.channels.LegacyChannelException) {
                                    acceptStatus = null
                                    acceptError = e.text
                                } catch (e: Throwable) {
                                    android.util.Log.w("ChannelInviteBubble", "publisher accept failed: ${e.javaClass.simpleName}")
                                    acceptStatus = null
                                    acceptError = me.lakko.app.chat.joinFailureCopy(e)
                                } finally {
                                    busy = false
                                }
                            }
                        },
                    )
                    acceptStatus?.let { line: me.lakko.app.l10n.UiText ->
                        Text(
                            line.text(),
                            style = MaterialTheme.typography.bodySmall,
                            color = contentColor?.copy(alpha = 0.8f) ?: MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    acceptError?.let { line: me.lakko.app.l10n.UiText ->
                        Text(line.text(), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
                    }
                }
                actions.isFollowed(f.channelIdHex) ->
                    CardButton(t("common.open"), true, bubbleColor, contentColor) { actions.openChannel(f.channelIdHex) }
                else -> CardButton(
                    label = if (busy) t("channels.subscribe.status.subscribing") else t("channels.subscribe.action"),
                    enabled = !busy,
                    bubbleColor = bubbleColor,
                    contentColor = contentColor,
                    onClick = {
                        busy = true
                        scope.launch {
                            try {
                                actions.subscribe(f)
                            } catch (e: kotlinx.coroutines.CancellationException) {
                                throw e
                            } catch (_: Throwable) {
                                // The link page states why; the card stays.
                            } finally {
                                busy = false
                            }
                        }
                    },
                )
            }
        }
    }
}

/** The card's one action: inside a bubble a pill in the bubble's text colour
 *  with the bubble's colour as its label, readable on both bubbles; outside
 *  one, a plain button. */
@Composable
private fun CardButton(
    label: String,
    enabled: Boolean,
    bubbleColor: androidx.compose.ui.graphics.Color?,
    contentColor: androidx.compose.ui.graphics.Color?,
    onClick: () -> Unit,
) {
    if (bubbleColor == null || contentColor == null) {
        Button(enabled = enabled, onClick = onClick) { Text(label) }
        return
    }
    androidx.compose.material3.Surface(
        onClick = onClick,
        enabled = enabled,
        shape = androidx.compose.foundation.shape.RoundedCornerShape(percent = 50),
        color = contentColor.copy(alpha = if (enabled) 1f else 0.6f),
        contentColor = bubbleColor,
    ) {
        Text(
            label,
            style = MaterialTheme.typography.labelLarge,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
        )
    }
}
