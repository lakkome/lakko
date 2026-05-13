// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.IdentitySecrets
import me.lakko.app.auth.base64Encode
import uniffi.lakko_ffi_uniffi.Identity

/**
 * In-memory state holder for the user's contacts surface.
 *
 * Wraps [ContactsClient] (1.15e5) and pairs it with the active
 * session's [IdentitySecrets] from [AuthRepository]. Exposes a
 * single [StateFlow] of [ContactsState] for the ViewModel to
 * `collectAsStateWithLifecycle` against.
 *
 * **No persistence yet** — pending requests + sent log live in
 * memory only. They drop on process death; on next launch the
 * receiver re-polls the stranger inbox from cursor `0` and
 * rebuilds the list. Room / DataStore-backed persistence lands
 * with 1.15f's chat surface (which needs MLS-state storage
 * anyway, so the two layers can share the same DB).
 *
 * **No continuous polling yet** — manual refresh via
 * [refreshInbox]. A polling loop lands in 1.15e7 alongside the
 * service-worker-style "fetch in the background" plumbing.
 *
 * ## Identity-secrets gate
 *
 * Every operation that signs / decrypts requires
 * [AuthRepository.identitySecrets] to be non-null. On a
 * session-restore path (rehydrate-from-disk), it is `null` until
 * the user re-enters their password. The repository surfaces
 * this state via [ContactsState.requiresReauth] so the screen
 * can render a "please re-enter password" panel instead of the
 * normal contacts list.
 */
public class ContactsRepository(
    private val authRepo: AuthRepository,
    private val clientFactory: () -> ContactsClient = { ContactsClient() },
) {
    private val mutableState: MutableStateFlow<ContactsState> =
        MutableStateFlow(ContactsState())

    public val state: StateFlow<ContactsState> = mutableState.asStateFlow()

    /**
     * Resolve the username typed in the search box, compose a
     * `ContactRequest`, publish it to the recipient's mailbox.
     *
     * On success the recipient gets the request in their
     * stranger inbox; we add an entry to [ContactsState.sent]
     * so the UI can render an outgoing-request row.
     *
     * Throws [ContactsError] on any failure — same shape as the
     * underlying [ContactsClient.sendContactRequest].
     */
    public suspend fun sendContactRequest(
        recipientUsername: String,
        introMessage: String,
    ): SendResult {
        val secrets = authRepo.identitySecrets.value
            ?: throw ContactsError.wrap(
                IllegalStateException(
                    "no identity secrets — please re-enter your password",
                ),
            )

        return useClientWith(secrets) { client, sender ->
            val result = client.sendContactRequest(
                senderSigningSeed = sender.signingSeed,
                senderAccountId = secrets.accountId,
                senderSignaturePubkey = sender.signaturePubkey,
                recipientUsername = recipientUsername,
                introMessage = introMessage.toByteArray(Charsets.UTF_8),
            )
            mutableState.update { state ->
                state.copy(
                    sent = state.sent + SentContactRequest(
                        recipientUsername = result.recipientUsername,
                        recipientAccountIdB64 = base64Encode(result.recipientAccountId),
                        messageId = result.messageId,
                        createdAt = result.createdAt,
                    ),
                )
            }
            result
        }
    }

    /**
     * Poll the stranger inbox once for new ContactRequest /
     * Welcome envelopes. Updates [ContactsState.pending] with
     * any new requests + bumps the cursor.
     *
     * Welcome envelopes are recognised but **dropped on the
     * floor** in 1.15e6 — turning them into a usable chat
     * session is 1.15f's job. They'll be re-polled then; the
     * cursor still advances here so we don't process them
     * twice, but for now they're a no-op.
     */
    public suspend fun refreshInbox() {
        val secrets = authRepo.identitySecrets.value ?: run {
            mutableState.update { it.copy(requiresReauth = true) }
            return
        }
        mutableState.update { it.copy(refreshing = true, lastError = null) }
        try {
            useClientWith(secrets) { client, sender ->
                val result = client.pollStrangerInbox(
                    mySignaturePubkey = sender.signaturePubkey,
                    myX25519Secret = sender.x25519Secret,
                    after = mutableState.value.cursor,
                    waitSecs = 0,
                )
                val newIncoming = result.events.filterIsInstance<
                    IncomingMailboxEvent.ContactRequest>(
                ).map { it.request }
                mutableState.update { state ->
                    state.copy(
                        pending = state.pending + newIncoming,
                        cursor = result.cursor,
                        refreshing = false,
                        lastError = null,
                    )
                }
            }
        } catch (e: ContactsError) {
            mutableState.update {
                it.copy(refreshing = false, lastError = e.message)
            }
        }
    }

    /**
     * Mark an incoming ContactRequest as accepted locally.
     *
     * In 1.15e6 this is **purely a local marker** — the
     * accept-side group creation (fetch recipient KeyPackage →
     * `Group::create` → `addMember` → `sendWelcome`) lives in
     * 1.15f's `ChatClient`. Production wires the MLS handshake
     * here; today the UI just dismisses the row from the
     * pending list.
     */
    public fun acceptContactRequest(messageId: Long) {
        mutableState.update { state ->
            val (accepted, remaining) = state.pending.partition { it.messageId == messageId }
            if (accepted.isEmpty()) return@update state
            state.copy(
                pending = remaining,
                accepted = state.accepted + accepted.map { AcceptedContact(it) },
            )
        }
    }

    /** Drop an incoming request from the pending list without
     *  acknowledging it to the sender. Mirror of the web
     *  Decline semantics — server doesn't get told, the row
     *  just disappears locally. */
    public fun declineContactRequest(messageId: Long) {
        mutableState.update { state ->
            state.copy(pending = state.pending.filterNot { it.messageId == messageId })
        }
    }

    /** Clear the in-memory state. Called from the AuthRepository
     *  when the session ends — pending requests + sent log do
     *  not persist across logouts. */
    public fun clear() {
        // Wipe any decrypted intro plaintext on the way out.
        // Best-effort: the `String` contents may still live in
        // heap if some collector cached them, but our reference
        // drops here.
        mutableState.value = ContactsState()
    }

    /**
     * Build a transient [Identity] from the stashed export key,
     * pass it + the live [ContactsClient] into [block], close
     * both on the way out.
     *
     * The Identity build is sub-millisecond (HKDF + Ed25519 +
     * X25519); a fresh handle per operation keeps the lifetime
     * story simple — no reference threading, no global state.
     * MLS state mutations would lose the throwaway provider, but
     * 1.15e6's operations are all stateless (sign + decrypt
     * only); the chat surface in 1.15f wires the canonical
     * Identity through `ChatProvider`.
     */
    private inline fun <T> useClientWith(
        secrets: IdentitySecrets,
        block: (ContactsClient, SenderMaterial) -> T,
    ): T {
        return clientFactory().use { client ->
            Identity.fromSeed(secrets.exportKey).use { identity ->
                val sender = SenderMaterial(
                    signingSeed = identity.ed25519Seed(),
                    signaturePubkey = identity.signaturePublicKey(),
                    x25519Secret = identity.x25519SecretKey(),
                )
                try {
                    block(client, sender)
                } finally {
                    sender.signingSeed.fill(0)
                    sender.x25519Secret.fill(0)
                }
            }
        }
    }

    /** Carrier for the bytes [ContactsClient] needs from the
     *  current session's Identity. The four byte arrays live on
     *  the heap for the duration of one repository call; the
     *  wipe in [useClientWith]'s finally narrows the window. */
    private data class SenderMaterial(
        val signingSeed: ByteArray,
        val signaturePubkey: ByteArray,
        val x25519Secret: ByteArray,
    )
}

// =============================================================================
// State shape
// =============================================================================

/** Snapshot of every field the [me.lakko.app.ui.contacts.ContactsScreen]
 *  renders. */
public data class ContactsState(
    /** Verified incoming requests waiting for Accept / Decline. */
    val pending: List<IncomingContactRequest> = emptyList(),
    /** Locally-accepted requests — in 1.15e6 these are inert
     *  (no MLS group has been created). 1.15f turns them into
     *  real chat sessions. */
    val accepted: List<AcceptedContact> = emptyList(),
    /** Outgoing requests we've sent in this session. Survives
     *  process death — there's no on-server "sent" mirror to
     *  rehydrate from, so a fresh process starts with an empty
     *  list. */
    val sent: List<SentContactRequest> = emptyList(),
    /** Cursor for the stranger-inbox poll. `0` = catch-up
     *  fetch on first refresh. */
    val cursor: Long = 0,
    /** Inbox refresh in progress — UI shows a spinner. */
    val refreshing: Boolean = false,
    /** Set when [AuthRepository.identitySecrets] is `null` —
     *  the screen surfaces a "re-enter your password" panel
     *  instead of the normal contacts list. */
    val requiresReauth: Boolean = false,
    /** Last error from a refresh / send attempt, for inline
     *  display. `null` clears on the next successful call. */
    val lastError: String? = null,
)

/** A request the user has accepted locally. 1.15f turns these
 *  into real MLS-backed chats. */
public data class AcceptedContact(
    val request: IncomingContactRequest,
)

/** Outgoing contact request, surfaced on the UI as an "I asked
 *  @<name> to connect" row. */
public data class SentContactRequest(
    val recipientUsername: String,
    val recipientAccountIdB64: String,
    val messageId: Long,
    val createdAt: String,
)
