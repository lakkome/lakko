// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle

/**
 * ADR-0101 section 7 — the channel invitations a return carried whose old
 * card binding did not verify under this account's previous key, drawn above
 * the chat list as the ordinary invitation card ([ChannelInviteBubbleCard]):
 * Subscribe, or Accept for a publisher invitation, by the person. Never
 * accepted silently, never dropped; an invitation leaves once the account
 * reads its channel. Mirror of `apps/web/src/channels/ReturnedInvitations.tsx`.
 */
@Composable
internal fun ReturnedInvitations(accountIdB64: String?) {
    val account = accountIdB64 ?: return
    val context = LocalContext.current.applicationContext
    val version by me.lakko.app.channels.ReturnedInvitationsStore.version.collectAsStateWithLifecycle()
    val store = remember(context) { me.lakko.app.channels.ReturnedInvitationsStore.onDevice(context) }
    val held = remember(version, account) { store.read(account) }
    val actions = ChannelInviteActionsHolder.actions ?: return
    val reading = held.keys.filter { actions.isFollowed(it) }.toSet()
    // An invitation whose channel the account reads now leaves the list.
    LaunchedEffect(reading, account) {
        if (reading.isNotEmpty()) store.drop(account, reading)
    }
    val shown = me.lakko.app.channels.invitationsToShow(held, reading)
    if (shown.isEmpty()) return
    Column(modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp)) {
        for (invitation in shown) {
            val card = remember(invitation.cardB64) {
                try {
                    java.util.Base64.getDecoder().decode(invitation.cardB64)
                } catch (_: IllegalArgumentException) {
                    null
                }
            } ?: continue
            ChannelInviteBubbleCard(
                text = "",
                card = card,
                messageId = invitation.channelIdHex.hashCode().toLong(),
            )
        }
    }
}
