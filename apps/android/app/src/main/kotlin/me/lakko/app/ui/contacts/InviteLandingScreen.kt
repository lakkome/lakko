// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.ui.components.LakkoAvatar

/**
 * The invite page (ADR-0079, `docs/ux/contact-invite-link.md` section 3): the
 * screen a tapped `https://lakko.me/i/<id>#<secret>` link lands on. Mirror of
 * the web `routes/Invite.tsx` and the iOS `InviteLandingScreen`.
 *
 * The inviter's name and username as the link states them, the label, **Add**
 * and **Not now**. No fingerprint, no safety number: those stay on the
 * contact's info screen. A signed-out person sees [SignedOutInviteScreen]
 * instead (UX section 3, "Signed out"); the deep link stays parked, and once
 * a session lands the page opens here in its default state.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun InviteLandingScreen(
    viewModel: InviteLandingViewModel,
    onDone: () -> Unit,
    /** The chat with the inviter: the pending chat after Add, the
     *  one-to-one (or the contact) when they are already a contact. */
    onOpenChat: (InviteChatTarget) -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val chatToOpen by viewModel.chatToOpen.collectAsStateWithLifecycle()
    LaunchedEffect(chatToOpen) { chatToOpen?.let(onOpenChat) }

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(t("invite.contact.page_title")) },
                navigationIcon = {
                    IconButton(onClick = onDone) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = t("common.back"))
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .padding(24.dp),
            verticalArrangement = Arrangement.Center,
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            when (val s = state) {
                InviteLandingState.Loading -> CircularProgressIndicator(modifier = Modifier.size(48.dp))
                is InviteLandingState.Ready -> ReadyPanel(s, onAdd = viewModel::add, onNotNow = onDone)
                is InviteLandingState.AlreadyContacts -> {
                    Title(t("invite.contact.page.already_title", mapOf("name" to nameOf(s.statedName))))
                    Spacer(Modifier.height(24.dp))
                    Button(onClick = { onOpenChat(s.target) }, modifier = Modifier.fillMaxWidth()) {
                        Text(t("invite.contact.page.open_chat"))
                    }
                }
                InviteLandingState.Own -> MessagePanel(
                    title = t("invite.contact.page.own_title"),
                    body = t("invite.contact.page.own_body"),
                    onDone = onDone,
                )
                InviteLandingState.Dead -> MessagePanel(
                    title = t("invite.contact.page.dead_title"),
                    body = t("invite.contact.page.dead_body"),
                    onDone = onDone,
                )
                InviteLandingState.RateLimited -> {
                    Text(
                        text = me.lakko.app.discovery.INVITE_RATE_LIMITED_TEXT.text(),
                        style = MaterialTheme.typography.bodyMedium,
                        textAlign = TextAlign.Center,
                    )
                    Spacer(Modifier.height(24.dp))
                    Button(onClick = viewModel::retryOpen) { Text(t("common.try_again")) }
                }
                InviteLandingState.OpenFailed -> {
                    Title(t("invite.contact.result.failed_title"))
                    Spacer(Modifier.height(24.dp))
                    Button(onClick = viewModel::retryOpen) { Text(t("common.try_again")) }
                }
                InviteLandingState.RequiresReauth -> MessagePanel(
                    title = t("chat.reauth.title"),
                    body = t("common.error.session_ended"),
                    onDone = onDone,
                )
            }
        }
    }
}

/**
 * The invite page for a person who is not signed in
 * (`docs/ux/contact-invite-link.md` section 3, "Signed out"; ADR-0079
 * reworked 2026-10-17): the same page, with **Sign in to add Anna** and
 * **Create an account**. Mirror of the web `Invite.tsx` signed-out state and
 * the iOS `SignedOutInviteScreen`. Opening it spends nothing and sends
 * nothing; the link stays parked, so after either choice the person returns
 * to the invite page in its default state, not to the chat list.
 */
@Composable
public fun SignedOutInviteScreen(
    slug: String,
    preview: suspend (String) -> me.lakko.app.contacts.InvitePreviewResult,
    onSignIn: () -> Unit,
    onCreateAccount: () -> Unit,
    onDone: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var attempt by androidx.compose.runtime.remember(slug) { androidx.compose.runtime.mutableIntStateOf(0) }
    val result by androidx.compose.runtime.produceState<me.lakko.app.contacts.InvitePreviewResult?>(
        initialValue = null,
        slug,
        attempt,
    ) {
        value = null
        value = preview(slug)
    }
    Column(
        modifier = modifier
            .fillMaxSize()
            .padding(24.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        when (val r = result) {
            null -> CircularProgressIndicator(modifier = Modifier.size(48.dp))
            is me.lakko.app.contacts.InvitePreviewResult.Preview -> {
                val name = nameOf(r.statedName)
                LakkoAvatar(
                    name = r.statedName.orEmpty(),
                    seed = me.lakko.app.ds.hexOf(r.ownerSignaturePubkey),
                    size = 72.dp,
                )
                Spacer(Modifier.height(16.dp))
                Title(t("invite.contact.page.title", mapOf("name" to name)))
                val username = r.username
                if (!username.isNullOrEmpty() && r.statedName != "@$username") {
                    Spacer(Modifier.height(4.dp))
                    Text(
                        text = "@$username",
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                if (r.label.isNotEmpty()) {
                    Spacer(Modifier.height(8.dp))
                    Text(
                        text = r.label,
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        textAlign = TextAlign.Center,
                    )
                }
                Spacer(Modifier.height(24.dp))
                Button(onClick = onSignIn, modifier = Modifier.fillMaxWidth()) {
                    Text(t("invite.contact.page.sign_in", mapOf("name" to name)))
                }
                Spacer(Modifier.height(8.dp))
                OutlinedButton(onClick = onCreateAccount, modifier = Modifier.fillMaxWidth()) {
                    Text(t("invite.contact.page.create_account"))
                }
            }
            me.lakko.app.contacts.InvitePreviewResult.Dead -> MessagePanel(
                title = t("invite.contact.page.dead_title"),
                body = t("invite.contact.page.dead_body"),
                onDone = onDone,
            )
            me.lakko.app.contacts.InvitePreviewResult.RateLimited -> {
                Text(
                    text = me.lakko.app.discovery.INVITE_RATE_LIMITED_TEXT.text(),
                    style = MaterialTheme.typography.bodyMedium,
                    textAlign = TextAlign.Center,
                )
                Spacer(Modifier.height(24.dp))
                Button(onClick = { attempt += 1 }) { Text(t("common.try_again")) }
            }
            me.lakko.app.contacts.InvitePreviewResult.Failed -> {
                Title(t("invite.contact.result.failed_title"))
                Spacer(Modifier.height(24.dp))
                Button(onClick = { attempt += 1 }) { Text(t("common.try_again")) }
            }
        }
    }
}

@Composable
private fun nameOf(stated: String?): String = stated ?: t("invite.contact.page.unnamed")

@Composable
private fun Title(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.headlineSmall,
        textAlign = TextAlign.Center,
    )
}

@Composable
private fun ReadyPanel(state: InviteLandingState.Ready, onAdd: () -> Unit, onNotNow: () -> Unit) {
    val invite = state.invite
    val name = nameOf(invite.statedName)
    LakkoAvatar(name = invite.statedName.orEmpty(), seed = me.lakko.app.ds.hexOf(invite.ownerAccountId), size = 72.dp)
    Spacer(Modifier.height(16.dp))
    Title(t("invite.contact.page.title", mapOf("name" to name)))
    val username = invite.username
    if (!username.isNullOrEmpty() && invite.statedName != "@$username") {
        Spacer(Modifier.height(4.dp))
        Text(
            text = "@$username",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
    if (invite.label.isNotEmpty()) {
        Spacer(Modifier.height(8.dp))
        Text(
            text = invite.label,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
        )
    }
    Spacer(Modifier.height(24.dp))
    Button(onClick = onAdd, enabled = !state.adding, modifier = Modifier.fillMaxWidth()) {
        if (state.adding) {
            CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
        } else {
            Text(t("invite.contact.page.add"))
        }
    }
    Spacer(Modifier.height(8.dp))
    OutlinedButton(onClick = onNotNow, enabled = !state.adding, modifier = Modifier.fillMaxWidth()) {
        Text(t("invite.contact.page.not_now"))
    }
    if (state.failed) {
        Spacer(Modifier.height(12.dp))
        Text(
            text = t("invite.contact.page.add_failed", mapOf("name" to name)),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.error,
            textAlign = TextAlign.Center,
        )
    }
}

@Composable
private fun MessagePanel(title: String, body: String, onDone: () -> Unit) {
    Title(title)
    Spacer(Modifier.height(8.dp))
    Text(
        text = body,
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        textAlign = TextAlign.Center,
    )
    Spacer(Modifier.height(24.dp))
    Button(onClick = onDone, modifier = Modifier.fillMaxWidth()) { Text(t("invite.contact.page.done")) }
}
