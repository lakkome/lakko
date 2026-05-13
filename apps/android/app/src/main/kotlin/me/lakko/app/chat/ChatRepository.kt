// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import java.util.Collections
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import me.lakko.app.auth.IdentitySecrets
import me.lakko.app.contacts.KIND_WELCOME
import me.lakko.app.contacts.wrapEnvelope
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.ds.DsClient
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.Group
import uniffi.lakko_ffi_uniffi.Identity

/**
 * In-memory + on-disk state holder for the user's MLS chats.
 *
 * **Owns the canonical [Identity]** for the current session —
 * built once via [bootstrap] from the OPAQUE export key, kept
 * alive until [clear] / [forgetDevice]. Every chat operation
 * threads this same handle through `ChatClient` so MLS state
 * mutations accumulate on a single OpenMLS provider. Throwaway
 * `Identity.fromSeed` per op (the simpler shape that
 * `ContactsRepository` uses for its stateless sign / decrypt
 * surface) would lose every ratchet advance.
 *
 * **Holds a per-group `Group` handle map** keyed by hex group
 * id. The Group instances carry MLS ratchet state; dropping
 * them mid-session would force a re-bootstrap from the
 * snapshot. The map is mutated under a coroutine [Mutex] so a
 * `sendMessage` and a parallel `pollAllGroups` don't race
 * against one another's [ChatClient] calls (the underlying
 * OpenMLS storage isn't internally synchronised for concurrent
 * writes against the same group).
 *
 * **Snapshot persistence** runs after every state-mutating call
 * via [IdentitySnapshotStore]. Failures are logged-and-swallowed
 * — the in-memory state is still correct; the next mutation
 * will retry the persist naturally.
 *
 * ## Lifecycle
 *
 *   * [bootstrap] is called by `AuthRepository.signup` /
 *     `login` / `recover` once the session's [IdentitySecrets]
 *     are known. It builds the canonical Identity from the
 *     export key and restores the on-disk snapshot if one
 *     exists. Idempotent for the same account id; replaces
 *     the Identity if called for a different account.
 *   * [clear] is called by `AuthRepository.logout` — drops the
 *     in-memory state but keeps the on-disk snapshot envelope
 *     intact, so the next login on this device picks up
 *     where we left off.
 *   * [forgetDevice] is called by `AuthRepository.forgetDevice`
 *     — additionally wipes the on-disk snapshot envelope; the
 *     only way back into the conversation history is from
 *     other devices' epochs forward (via MLS commits) once
 *     they catch up to this device's absence.
 *
 * 1.15f4b adds the Compose ChatScreen + ChatViewModel on top of
 * this; 1.15f4c wires `ContactsRepository.acceptContactRequest`
 * to call [startGroupWith] so an Accept turns into a real chat.
 */
public class ChatRepository(
    private val applicationContext: Context,
    private val chatClient: ChatClient = ChatClient(),
    private val dsClient: DsClient = DsClient(),
    private val discovery: DiscoveryClient = DiscoveryClient(),
    private val snapshotStore: IdentitySnapshotStore = IdentitySnapshotStore,
) : AutoCloseable {

    /**
     * The canonical [Identity] for the current session. `null`
     * until [bootstrap] populates it. Holding the reference in
     * an [AtomicReference] means snapshot writes don't need to
     * lock the whole repository — the [Identity] handle is
     * thread-safe at the UniFFI level (Rust-side `Arc`).
     */
    private val canonicalIdentity = AtomicReference<Identity?>(null)

    /**
     * Account id (base64) of the session that bootstrapped this
     * repository. Used to key the on-disk snapshot envelope.
     * Wrapped in `AtomicReference` so reads from any thread
     * pick up the value the bootstrap coroutine wrote.
     */
    private val currentAccountIdB64 = AtomicReference<String?>(null)

    /**
     * Map of hex group id → live [Group] handle. Mutated under
     * [groupsLock] for the same-group operations; reads can be
     * lock-free via the snapshot returned by `Collections.unmodifiableMap`.
     */
    private val groups: MutableMap<String, GroupRecord> =
        Collections.synchronizedMap(mutableMapOf())

    /**
     * Coroutine mutex for serialising MLS state mutations on a
     * single group. Coarse-grained: one mutex per repository,
     * not per group. Phase-1 chat volume doesn't justify the
     * per-group fine-grained map; if a future chat fan-out
     * ever pushes back on this, swap in a
     * `ConcurrentHashMap<String, Mutex>`.
     */
    private val groupsLock = Mutex()

    private val mutableState: MutableStateFlow<ChatRepositoryState> =
        MutableStateFlow(ChatRepositoryState())

    public val state: StateFlow<ChatRepositoryState> = mutableState.asStateFlow()

    /**
     * Build the canonical Identity for the session from the
     * OPAQUE export key + restore any on-disk MLS snapshot.
     *
     * Idempotent for the same account id (early-returns).
     * Switching accounts (different `secrets.accountId`) wipes
     * the in-memory state first.
     *
     * Failures from snapshot restore are logged-and-swallowed:
     * a corrupted / stale envelope shouldn't block the user
     * from continuing — they just start with an empty
     * conversation list on this device, and peers re-bootstrap
     * via fresh KeyPackages.
     */
    public fun bootstrap(secrets: IdentitySecrets) {
        val newAccountIdB64 = base64UrlSafe(secrets.accountId)
        val existing = currentAccountIdB64.get()
        if (existing == newAccountIdB64) return
        if (existing != null) {
            // Different account — wipe in-memory state before
            // attaching the new Identity.
            wipeInMemoryState()
        }
        val identity = Identity.fromSeed(secrets.exportKey)
        // Restore the snapshot — best-effort. The Identity is
        // valid even if the restore fails (we just lose the
        // prior MLS state, peers re-bootstrap).
        runCatching {
            val blob = snapshotStore.getSnapshot(applicationContext, newAccountIdB64)
            if (blob != null) {
                identity.restoreStorage(blob)
                // `restoreStorage` mutated the OpenMLS provider
                // storage; we still need to figure out which
                // groups existed in the snapshot. 1.15f4 keeps
                // the per-group `Group` rebuild lazy: peers'
                // commits will surface via pollAllGroups and the
                // user's send-side path will Group.create or
                // joinFromWelcome on demand. A future sprint
                // could iterate the provider's known group ids
                // and rebuild handles eagerly.
            }
        }
        canonicalIdentity.set(identity)
        currentAccountIdB64.set(newAccountIdB64)
        mutableState.update { it.copy(bootstrapped = true) }
    }

    /**
     * Start a 1×1 MLS group with [recipientAccountId].
     *
     * Steps:
     *   1. Fetch the recipient's KeyPackage from the discovery
     *      directory.
     *   2. `Group.create(identity)` — fresh group with us as the
     *      founding member.
     *   3. `group.addMemberFromWire(identity, keyPackageBytes)`
     *      — produces a `(commit, welcome)` pair. Per MLS the
     *      commit would be broadcast, but our group is
     *      one-of-one before this call, so we drop it.
     *   4. Wrap the welcome with [KIND_WELCOME] and post it to
     *      the recipient's stranger inbox via
     *      `DsClient.publishMailbox` keyed by
     *      `IdentityPseudonym.fromIdentity(recipientPubkey)`.
     *   5. Persist the MLS snapshot.
     *
     * Returns the new group's hex id so the caller can hand it
     * straight to the Chat UI route.
     *
     * @throws IllegalStateException when [bootstrap] hasn't
     *         been called yet.
     */
    public suspend fun startGroupWith(
        recipientAccountId: ByteArray,
        recipientSignaturePubkey: ByteArray,
    ): String {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )

        val keyPackageBytes = discovery.fetchKeyPackage(recipientAccountId)
            ?: throw ChatError(
                "recipient has no KeyPackage uploaded yet",
                IllegalStateException("fetchKeyPackage returned null"),
            )

        return groupsLock.withLock {
            val group = Group.create(identity)
            val addResult = try {
                group.addMemberFromWire(identity, keyPackageBytes)
            } catch (e: FfiException) {
                throw ChatError("addMemberFromWire failed", e)
            }
            // commit dropped — group is one-of-one before the
            // add, no broadcast needed.

            val welcomeEnvelope = wrapEnvelope(KIND_WELCOME, addResult.welcome)
            val recipientPseudonym =
                uniffi.lakko_ffi_uniffi.IdentityPseudonym
                    .fromIdentity(recipientSignaturePubkey)
                    .use { it.asBytes() }
            try {
                dsClient.publishMailbox(recipientPseudonym, welcomeEnvelope)
            } catch (e: Throwable) {
                throw ChatError("DS publishMailbox (Welcome) failed", e)
            }

            val groupId = group.groupId()
            val groupIdHex = DsClient.pseudonymHex(groupId)
            groups[groupIdHex] = GroupRecord(group = group, groupId = groupId)
            persistSnapshotBestEffort()
            publishThreadList()
            groupIdHex
        }
    }

    /**
     * Join an MLS group from a `Welcome` blob (received via
     * `ContactsRepository.pollStrangerInbox` → forwarded by
     * `IncomingMailboxEvent.Welcome`).
     *
     * Returns the joined group's hex id.
     *
     * @throws IllegalStateException when [bootstrap] hasn't
     *         been called yet.
     */
    public suspend fun joinGroupFromWelcome(welcomeBytes: ByteArray): String {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException(
                "ChatRepository not bootstrapped — call bootstrap() first",
            )
        return groupsLock.withLock {
            val group = try {
                Group.joinFromWelcome(welcomeBytes, identity)
            } catch (e: FfiException) {
                throw ChatError("joinFromWelcome failed", e)
            }
            val groupId = group.groupId()
            val groupIdHex = DsClient.pseudonymHex(groupId)
            groups[groupIdHex] = GroupRecord(group = group, groupId = groupId)
            persistSnapshotBestEffort()
            publishThreadList()
            groupIdHex
        }
    }

    /**
     * Encrypt [plaintext] for the current epoch of [groupIdHex]
     * and post the wire bytes to the DS. Appends the message to
     * the in-memory timeline + persists the snapshot.
     */
    public suspend fun sendMessage(groupIdHex: String, plaintext: String) {
        val identity = canonicalIdentity.get()
            ?: throw IllegalStateException("ChatRepository not bootstrapped")
        val record = groups[groupIdHex]
            ?: throw ChatError(
                "no live Group handle for $groupIdHex",
                IllegalStateException("unknown group"),
            )

        groupsLock.withLock {
            val result = chatClient.sendMessage(
                identity = identity,
                group = record.group,
                plaintext = plaintext.toByteArray(Charsets.UTF_8),
            )
            record.appendOutgoing(plaintext, result.messageId, result.createdAt)
            persistSnapshotBestEffort()
            publishThreadList()
        }
    }

    /**
     * Pull new messages from one group's journal + apply them
     * to the in-memory timeline.
     */
    public suspend fun pollGroup(groupIdHex: String) {
        val identity = canonicalIdentity.get() ?: return
        val record = groups[groupIdHex] ?: return

        groupsLock.withLock {
            val result = chatClient.pollMessages(
                identity = identity,
                group = record.group,
                after = record.cursor,
                waitSecs = 0,
            )
            record.applyPollResult(result)
            if (result.events.isNotEmpty()) {
                persistSnapshotBestEffort()
            }
            publishThreadList()
        }
    }

    /** Pull new messages for every active group. Best-effort:
     *  per-group failures are logged-and-swallowed so one bad
     *  apple doesn't block the others. */
    public suspend fun pollAllGroups() {
        val identity = canonicalIdentity.get() ?: return
        // Snapshot the keys outside the lock — pollGroup
        // re-acquires it per group.
        val groupIds = synchronized(groups) { groups.keys.toList() }
        for (groupIdHex in groupIds) {
            try {
                pollGroup(groupIdHex)
            } catch (_: Throwable) {
                // Best-effort: the UI surfaces a per-thread error
                // when 1.15f4b wires that — for now silence is
                // acceptable.
            }
        }
    }

    /**
     * Clear the in-memory state for the current session. Keeps
     * the on-disk MLS snapshot intact — `forgetDevice` is the
     * full-wipe path. Called from `AuthRepository.logout`.
     */
    public fun clear() {
        wipeInMemoryState()
        currentAccountIdB64.set(null)
        canonicalIdentity.set(null)
        mutableState.value = ChatRepositoryState()
    }

    /**
     * Full wipe: clear in-memory state + delete the on-disk
     * snapshot envelope. Called from
     * `AuthRepository.forgetDevice`.
     */
    public fun forgetDevice() {
        val accountIdB64 = currentAccountIdB64.get()
        clear()
        if (accountIdB64 != null) {
            try {
                snapshotStore.deleteSnapshot(applicationContext, accountIdB64)
            } catch (_: Throwable) {
                // Best-effort wipe; the user has already
                // forgotten the device, missing snapshot
                // delete is non-fatal.
            }
        }
    }

    override fun close() {
        chatClient.close()
        dsClient.close()
        discovery.close()
        canonicalIdentity.getAndSet(null)?.close()
        synchronized(groups) {
            for (rec in groups.values) rec.group.close()
            groups.clear()
        }
    }

    // -----------------------------------------------------------------
    // Internals
    // -----------------------------------------------------------------

    private fun wipeInMemoryState() {
        synchronized(groups) {
            for (rec in groups.values) rec.group.close()
            groups.clear()
        }
        // Close the previous identity handle (releases the
        // Rust-side Arc + provider storage); the next
        // bootstrap builds a fresh one.
        canonicalIdentity.getAndSet(null)?.close()
    }

    /** Save the canonical Identity's MLS snapshot, swallowing
     *  any failure. The in-memory state stays good — the next
     *  mutation will retry the persist naturally. */
    private fun persistSnapshotBestEffort() {
        val identity = canonicalIdentity.get() ?: return
        val accountIdB64 = currentAccountIdB64.get() ?: return
        try {
            val blob = identity.snapshotStorage()
            snapshotStore.putSnapshot(applicationContext, accountIdB64, blob)
        } catch (_: Throwable) {
            // Best-effort. 1.15d7 will plumb a `tracing`-
            // equivalent logger so failures here surface to
            // diagnostics.
        }
    }

    /** Snapshot the per-group state into the public StateFlow.
     *  Called after every mutation. */
    private fun publishThreadList() {
        val threads = synchronized(groups) {
            groups.values.map { it.toThread() }
        }
        mutableState.update { it.copy(threads = threads) }
    }

    /** URL-safe-ish base64 of the 16-byte account id, used as
     *  the filename for [IdentitySnapshotStore]. */
    private fun base64UrlSafe(accountId: ByteArray): String =
        me.lakko.app.auth.base64Encode(accountId)
}

// =============================================================================
// State shape
// =============================================================================

/** Snapshot of every ChatRepository field a UI cares about. */
public data class ChatRepositoryState(
    val threads: List<ChatThread> = emptyList(),
    val bootstrapped: Boolean = false,
)

/** One conversation thread — keyed by hex group id. */
public data class ChatThread(
    /** Hex-encoded MLS group id; used as the DS journal key
     *  and as the route argument for the 1×1 ChatScreen. */
    val groupIdHex: String,
    val messages: List<ChatMessage>,
    /** Cursor for the next [ChatClient.pollMessages] call. */
    val cursor: Long,
)

/** One message in a [ChatThread]. */
public data class ChatMessage(
    /** Server-assigned DS id (or 0 for an outgoing message not
     *  yet acknowledged by the DS — `sendMessage` populates
     *  this before the call returns, so in practice we always
     *  carry the real id). */
    val messageId: Long,
    /** Direction marker. `Outgoing` for messages we sent;
     *  `Incoming` for everything from a peer; `System` for
     *  MLS commit / proposal frames. */
    val direction: Direction,
    /** UTF-8 plaintext (empty for system rows). */
    val text: String,
    /** ISO-8601 timestamp from the DS. */
    val createdAt: String,
)

public enum class Direction { Outgoing, Incoming, System }

/** Internal per-group record holding the live MLS handle +
 *  the timeline mutations the StateFlow surfaces.
 *
 *  Not a `data class` because the `Group` handle is a mutable
 *  UniFFI Object and shouldn't participate in `equals` /
 *  `hashCode`. */
private class GroupRecord(
    val group: Group,
    val groupId: ByteArray,
) {
    private val messages: MutableList<ChatMessage> =
        Collections.synchronizedList(mutableListOf())

    @Volatile
    var cursor: Long = 0
        private set

    fun appendOutgoing(text: String, messageId: Long, createdAt: String) {
        messages.add(
            ChatMessage(
                messageId = messageId,
                direction = Direction.Outgoing,
                text = text,
                createdAt = createdAt,
            ),
        )
    }

    fun applyPollResult(result: PollMessagesResult) {
        for (event in result.events) {
            when (event) {
                is IncomingChatEvent.Application -> messages.add(
                    ChatMessage(
                        messageId = event.messageId,
                        direction = Direction.Incoming,
                        text = event.plaintext,
                        createdAt = event.createdAt,
                    ),
                )
                is IncomingChatEvent.System -> messages.add(
                    ChatMessage(
                        messageId = event.messageId,
                        direction = Direction.System,
                        text = systemRowText(event.systemKind),
                        createdAt = event.createdAt,
                    ),
                )
            }
        }
        cursor = result.cursor
    }

    fun toThread(): ChatThread = ChatThread(
        groupIdHex = DsClient.pseudonymHex(groupId),
        messages = messages.toList(),
        cursor = cursor,
    )

    private fun systemRowText(kind: SystemKind): String = when (kind) {
        SystemKind.Commit -> "[mls commit]"
        SystemKind.Proposal -> "[mls proposal]"
        SystemKind.ExternalJoinProposal -> "[mls external join]"
    }
}
