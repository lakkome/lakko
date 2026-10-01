// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import android.util.Log
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import me.lakko.app.chat.IdentitySnapshotStore
import me.lakko.app.chat.MainThreadGuard
import uniffi.lakko_ffi_uniffi.Identity

/**
 * The ONE long-lived MLS identity the channel publisher paths run on, with
 * its OpenMLS provider snapshotted to disk and restored on bind.
 *
 * ## Why this exists
 *
 * A channel's PublisherGroup (ADR-0010 Tier 1) is an MLS group that
 * `PublisherGroup::create` persists into `identity.provider.storage()` — the
 * in-memory OpenMLS provider of whichever `Identity` object the caller passed
 * in. The channel screens used to build a throwaway
 * `Identity.fromSeed(accountIdentitySeed)` per operation and never snapshot
 * it, so the group lived exactly as long as that object: after a relaunch
 * `publisherGroupOpenOrCreate` found nothing and minted a FRESH group with a
 * different content-key seed. A creator could then not post under the seed
 * its subscribers hold, and every post made before the relaunch became
 * unreadable to the creator. `ChatRepository` solved the same problem for
 * chats by snapshotting its canonical identity's provider after every
 * mutation; this class does the same for the channel provider, kept
 * SEPARATE from the chat one so neither snapshot can roll the other's groups
 * back (the ADR-0040 sibling-rollback hazard).
 *
 * ## Discipline
 *
 *   * Every FFI call on the identity or on a `PublisherGroup` opened from it
 *     goes through [withIdentity]: one [Mutex] serialises them (so a
 *     snapshot never captures a torn provider) and they run on
 *     `Dispatchers.Default`, never the main thread (the `MainThreadGuard`
 *     tripwire fires in a debug build otherwise).
 *   * A block that completes normally with `persist = true` is followed by a
 *     provider snapshot to disk. A block that THROWS is not persisted: a
 *     local commit whose publish failed would otherwise become a durable
 *     device-ahead epoch; un-persisted, a relaunch returns to the last state
 *     the peers agreed on.
 *   * The snapshot is wrapped by [IdentitySnapshotStore]'s Keystore key under
 *     a channel-scoped record (`<accountIdB64>.channels`), the same at-rest
 *     posture as the chat snapshot.
 *
 * The identity is derived from the account's phrase seed (ADR-0017), the same
 * key the chat identity uses; that was already the case for the throwaway
 * objects this replaces.
 */
public class ChannelPublisherIdentity(
    private val loadSnapshot: (accountIdB64: String) -> ByteArray?,
    private val saveSnapshot: (accountIdB64: String, snapshot: ByteArray) -> Unit,
) {
    public companion object {
        private const val TAG = "ChannelPublisherIdentity"

        /** The channel-scoped record name inside the chat snapshot store. */
        private const val SNAPSHOT_SCOPE_SUFFIX = ".channels"

        /** The on-device instance: snapshots ride the Keystore-wrapped
         *  [IdentitySnapshotStore] under a channel-scoped record. */
        public fun onDevice(context: Context): ChannelPublisherIdentity {
            val app = context.applicationContext
            return ChannelPublisherIdentity(
                loadSnapshot = { acct -> IdentitySnapshotStore.getSnapshot(app, acct + SNAPSHOT_SCOPE_SUFFIX) },
                saveSnapshot = { acct, bytes -> IdentitySnapshotStore.putSnapshot(app, acct + SNAPSHOT_SCOPE_SUFFIX, bytes) },
            )
        }

        /** Forget device: delete [accountIdB64]'s on-device provider snapshot.
         *  The one caller is [me.lakko.app.account.AccountLocalWipe]; the
         *  in-memory identity is closed by [unbind]. */
        public fun deleteOnDeviceSnapshot(context: Context, accountIdB64: String) {
            IdentitySnapshotStore.deleteSnapshot(context.applicationContext, accountIdB64 + SNAPSHOT_SCOPE_SUFFIX)
        }
    }

    /** Thrown by [withIdentity] before [bind] has run (or after [unbind]). */
    public class NotBoundException : IllegalStateException("channel publisher identity is not bound")

    private val mutex = Mutex()
    private var identity: Identity? = null
    private var boundAccountIdB64: String? = null

    /** The account the identity is bound to, or `null`. */
    public val boundAccount: String?
        get() = boundAccountIdB64

    /**
     * Build the identity for [accountIdB64] from [accountIdentitySeed] and
     * restore its provider from the persisted snapshot (when one exists).
     * Idempotent for the same account; a different account replaces the
     * identity. Runs off the main thread.
     */
    public suspend fun bind(accountIdB64: String, accountIdentitySeed: ByteArray) {
        withContext(Dispatchers.Default) {
            mutex.withLock {
                if (boundAccountIdB64 == accountIdB64 && identity != null) return@withLock
                MainThreadGuard.assertOffMain("ChannelPublisherIdentity.bind")
                identity?.close()
                identity = null
                boundAccountIdB64 = null
                val fresh = Identity.fromSeed(accountIdentitySeed)
                val snapshot = try {
                    loadSnapshot(accountIdB64)
                } catch (e: Throwable) {
                    // An unreadable snapshot reads as "none": the publisher
                    // groups then re-mint on next open, the pre-persistence
                    // behaviour, rather than the whole Channels tab failing.
                    Log.w(TAG, "channel provider snapshot unreadable; starting empty", e)
                    null
                }
                if (snapshot != null && snapshot.isNotEmpty()) {
                    try {
                        fresh.restoreStorage(snapshot)
                    } catch (e: Throwable) {
                        Log.w(TAG, "channel provider snapshot not restored; starting empty", e)
                    }
                }
                identity = fresh
                boundAccountIdB64 = accountIdB64
            }
        }
    }

    /**
     * Run [block] on the bound identity, serialised with every other caller
     * and off the main thread. With [persist] the provider is snapshotted
     * after a block that returns normally; a throwing block persists nothing.
     *
     * The mutex is held for the block's whole duration, network calls
     * included — the callers are the channel owner's own background
     * operations (a rotation, a co-publisher approve, the journal poll) and a
     * user action that shows a busy state meanwhile; nothing under the lock
     * awaits another holder of it.
     */
    public suspend fun <T> withIdentity(persist: Boolean = true, block: suspend (Identity) -> T): T =
        withContext(Dispatchers.Default) {
            mutex.withLock {
                MainThreadGuard.assertOffMain("ChannelPublisherIdentity.withIdentity")
                val id = identity ?: throw NotBoundException()
                val result = block(id)
                if (persist) persistLocked(id)
                result
            }
        }

    /** Snapshot the provider now (under the lock). A no-op when unbound. */
    public suspend fun persist() {
        withContext(Dispatchers.Default) {
            mutex.withLock {
                val id = identity ?: return@withLock
                persistLocked(id)
            }
        }
    }

    /** Close the identity; the snapshot on disk stays. Logout. */
    public fun unbind() {
        identity?.close()
        identity = null
        boundAccountIdB64 = null
    }

    private fun persistLocked(id: Identity) {
        val acct = boundAccountIdB64 ?: return
        try {
            val bytes = id.snapshotStorage()
            if (bytes.isNotEmpty()) saveSnapshot(acct, bytes)
            bytes.fill(0)
        } catch (e: Throwable) {
            // Best-effort like the chat persist: the in-memory state is
            // authoritative for this session and the next successful
            // persist captures everything since.
            Log.w(TAG, "channel provider snapshot not persisted", e)
        }
    }
}
