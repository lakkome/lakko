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
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.contacts.InvitePreviewResult
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
 * instead (UX section 3, "Signed out", amended 2026-10-26), with the deep link
 * parked; once a session lands the link is checked again here. Signed in, a
 * link that does not work (revoked, expired, used up, unknown, damaged) has
 * no page: the person is taken to Contacts, and so is a person whose Add
 * found the link dead.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun InviteLandingScreen(
    viewModel: InviteLandingViewModel,
    onDone: () -> Unit,
    /** The chat with the inviter: the pending chat after Add, the
     *  one-to-one (or the contact) when they are already a contact. */
    onOpenChat: (InviteChatTarget) -> Unit,
    /** A link that does not work: the Contacts screen, nothing else shown
     *  (amended 2026-10-19). */
    onOpenContacts: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val chatToOpen by viewModel.chatToOpen.collectAsStateWithLifecycle()
    LaunchedEffect(chatToOpen) { chatToOpen?.let(onOpenChat) }
    LaunchedEffect(state) { if (state == InviteLandingState.Dead) onOpenContacts() }

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
                is InviteLandingState.AlreadyContacts -> KnownContactPanel(s, onOpenChat = viewModel::openChat)
                InviteLandingState.Own -> MessagePanel(
                    title = t("invite.contact.page.own_title"),
                    body = t("invite.contact.page.own_body"),
                    onDone = onDone,
                )
                // No page: the screen goes to Contacts (see above).
                InviteLandingState.Dead -> Unit
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
 * (`docs/ux/contact-invite-link.md` section 3, "Signed out", amended
 * 2026-10-26; ADR-0079 amendment of 2026-10-26): who invites, one line about
 * Lakko, **Create an account** and **I already have an account**. A link that
 * no longer works says so, with the same two buttons. Opening it spends
 * nothing and sends nothing: the card is opened on the device from the
 * ledger's sealed blob, fetched with no session.
 *
 * The caller keeps the link parked for a live link (the session that lands
 * opens it again) and drops it for a dead one ([keepsLinkParked]).
 */
@Composable
public fun SignedOutInviteScreen(
    slug: String,
    preview: suspend (String) -> InvitePreviewResult,
    /** An account button: the chosen state says whether the link stays parked. */
    onCreateAccount: (SignedOutInviteState) -> Unit,
    onHaveAccount: (SignedOutInviteState) -> Unit,
    modifier: Modifier = Modifier,
) {
    var attempt by remember(slug) { mutableIntStateOf(0) }
    val state by produceState<SignedOutInviteState>(SignedOutInviteState.Loading, slug, attempt) {
        value = SignedOutInviteState.Loading
        value = try {
            signedOutInviteState(preview(slug))
        } catch (err: Throwable) {
            me.lakko.app.rethrowIfCancelled(err)
            android.util.Log.w("LakkoInvite", "WARN signed-out preview threw: ${err.javaClass.simpleName}")
            SignedOutInviteState.OpenFailed
        }
    }
    Column(
        modifier = modifier
            .fillMaxSize()
            .padding(24.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        when (val s = state) {
            SignedOutInviteState.Loading -> CircularProgressIndicator(modifier = Modifier.size(48.dp))
            is SignedOutInviteState.Live -> {
                val name = nameOf(s.statedName)
                LakkoAvatar(name = s.statedName.orEmpty(), seed = s.avatarSeed, size = 72.dp)
                Spacer(Modifier.height(16.dp))
                Title(t("invite.contact.page.signed_out_title", mapOf("name" to name)))
                if (s.usernameLine != null) {
                    Spacer(Modifier.height(4.dp))
                    Text(
                        text = s.usernameLine,
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                if (s.label.isNotEmpty()) {
                    Spacer(Modifier.height(8.dp))
                    Text(
                        text = s.label,
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        textAlign = TextAlign.Center,
                    )
                }
                Spacer(Modifier.height(16.dp))
                Text(
                    text = t("invite.contact.page.about"),
                    style = MaterialTheme.typography.bodyMedium,
                    textAlign = TextAlign.Center,
                )
                AccountButtons(onCreateAccount = { onCreateAccount(s) }, onHaveAccount = { onHaveAccount(s) })
            }
            SignedOutInviteState.Dead -> {
                Title(t("invite.contact.page.dead_title"))
                Spacer(Modifier.height(8.dp))
                Text(
                    text = t("invite.contact.page.dead_body"),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
                AccountButtons(onCreateAccount = { onCreateAccount(s) }, onHaveAccount = { onHaveAccount(s) })
            }
            SignedOutInviteState.RateLimited -> {
                Text(
                    text = me.lakko.app.discovery.INVITE_RATE_LIMITED_TEXT.text(),
                    style = MaterialTheme.typography.bodyMedium,
                    textAlign = TextAlign.Center,
                )
                Spacer(Modifier.height(24.dp))
                Button(onClick = { attempt += 1 }) { Text(t("common.try_again")) }
            }
            SignedOutInviteState.OpenFailed -> {
                Title(t("invite.contact.result.failed_title"))
                Spacer(Modifier.height(24.dp))
                Button(onClick = { attempt += 1 }) { Text(t("common.try_again")) }
            }
        }
    }
}

@Composable
private fun AccountButtons(onCreateAccount: () -> Unit, onHaveAccount: () -> Unit) {
    Spacer(Modifier.height(24.dp))
    Button(onClick = onCreateAccount, modifier = Modifier.fillMaxWidth()) {
        Text(t("invite.contact.page.create_account"))
    }
    Spacer(Modifier.height(8.dp))
    OutlinedButton(onClick = onHaveAccount, modifier = Modifier.fillMaxWidth()) {
        Text(t("invite.contact.page.have_account"))
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
    LakkoAvatar(
        name = invite.statedName.orEmpty(),
        seed = me.lakko.app.ds.hexOf(invite.ownerAccountId),
        size = 72.dp,
        // ADR-0084 — the link's preview carries no photo; one this device
        // already holds for the inviter shows.
        personKey = invite.ownerSignaturePubkey,
    )
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
    // ADR-0089 step 4 / UX 3 — a link from an account the moderation log
    // confirms suspended has no Add.
    val inviterKeyB64 = remember(invite.ownerSignaturePubkey) {
        invite.ownerSignaturePubkey.takeIf { it.size == 32 }?.let { java.util.Base64.getEncoder().encodeToString(it) }
    }
    val inviterSuspended by me.lakko.app.ui.suspension.rememberAccountSuspended(inviterKeyB64)
    // ADR-0089 section 4 - a suspended account adds no contacts.
    val addReadOnly by me.lakko.app.auth.SuspensionGate.readOnly.collectAsStateWithLifecycle()
    if (inviterSuspended || state.inviterSuspended) {
        Text(
            t("suspended.cannot_add"),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
        )
        Spacer(Modifier.height(8.dp))
    } else if (addReadOnly) {
        Text(
            t("suspended.self.body"),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
        )
        Spacer(Modifier.height(8.dp))
    } else Button(onClick = onAdd, enabled = !state.adding, modifier = Modifier.fillMaxWidth()) {
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

/**
 * "Anna is already in your contacts" and **Open chat** (UX section 3, amended
 * 2026-10-24). The link still delivers its answer, so an inviter who removed
 * this person and sent a new link gets them back; the button shows a spinner
 * while it runs, and the chat opens once the answer is written, or at once
 * when the link no longer works.
 */
@Composable
private fun KnownContactPanel(state: InviteLandingState.AlreadyContacts, onOpenChat: () -> Unit) {
    val name = nameOf(state.statedName)
    Title(t("invite.contact.page.already_title", mapOf("name" to name)))
    Spacer(Modifier.height(24.dp))
    Button(onClick = onOpenChat, enabled = !state.answering, modifier = Modifier.fillMaxWidth()) {
        if (state.answering) {
            CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
        } else {
            Text(t("invite.contact.page.open_chat"))
        }
    }
    val failure = state.failure
    if (failure != null) {
        Spacer(Modifier.height(12.dp))
        Text(
            text = when (failure) {
                AnswerFailure.RATE_LIMITED -> me.lakko.app.discovery.INVITE_RATE_LIMITED_TEXT.text()
                AnswerFailure.FAILED -> t("invite.contact.page.answer_failed", mapOf("name" to name))
            },
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
