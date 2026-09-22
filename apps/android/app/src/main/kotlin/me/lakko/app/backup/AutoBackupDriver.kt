// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import android.content.Context
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import me.lakko.app.contacts.AcceptedContactsStore
import uniffi.lakko_ffi_uniffi.BackupContactRecord

/**
 * Runs the automatic backup: watches for changes worth keeping, and flushes
 * them on a slow tick.
 *
 * Mirror of the web `backup/auto-backup-driver.ts` and the iOS
 * `AutoBackupDriver.swift`, down to where the membership check lives. It is
 * asked HERE, at the tick, rather than hooked onto the snapshot write: that
 * path runs only after an MLS mutation, so a device that had already joined
 * everything and merely opened the app never marked itself dirty - precisely
 * the person whose backup is most stale. Asking at the tick also makes the
 * first run after install a change, because nothing on this device can vouch
 * for what the server already holds.
 */
public class AutoBackupDriver(
    private val applicationContext: Context,
    private val scope: CoroutineScope,
    private val inputs: Inputs,
) {

    /**
     * Everything a run needs from the session, read at each tick because a
     * session token is rebound and a snapshot moves.
     */
    public data class Inputs(
        val accountId: () -> ByteArray?,
        val accountIdB64: () -> String?,
        val sessionToken: () -> String?,
        /** The canonical AccountIdentity seed. */
        val accountIdentitySeed: () -> ByteArray?,
        /**
         * The groups this device knows about, or `null` before the chat layer
         * has loaded them - which is not the same as "no groups" and must not
         * be read as a departure from every one of them.
         */
        val knownGroupIds: () -> List<String>?,
        /** The whole MLS storage snapshot, or `null` when no identity is
         *  loaded. Suspending because it is taken under the provider lock. */
        val snapshot: suspend () -> ByteArray?,
    )

    public enum class Outcome { CLEAN, THROTTLED, NOT_READY, UPLOADED, FAILED }

    private var loop: Job? = null

    /**
     * Start ticking. Idempotent - a second call is a no-op, so a recreated
     * activity does not stack loops.
     */
    public fun start() {
        if (loop?.isActive == true) return
        loop = scope.launch {
            while (isActive) {
                runOnce()
                delay(AutoBackup.TICK_MS)
            }
        }
    }

    public fun stop() {
        loop?.cancel()
        loop = null
    }

    /** One pass. Exposed so a test can drive it without a clock. */
    public suspend fun runOnce(nowMs: Long = System.currentTimeMillis()): Outcome {
        val accountIdB64 = inputs.accountIdB64() ?: return Outcome.NOT_READY

        val groups = inputs.knownGroupIds()
        if (groups != null &&
            AutoBackupStore.knownGroupsChanged(applicationContext, accountIdB64, groups)
        ) {
            AutoBackupStore.markDirty(applicationContext, accountIdB64, nowMs)
        }

        val state = AutoBackupStore.load(applicationContext, accountIdB64)
        when (AutoBackup.verdict(state, nowMs)) {
            AutoBackup.Verdict.CLEAN -> return Outcome.CLEAN
            AutoBackup.Verdict.THROTTLED -> return Outcome.THROTTLED
            AutoBackup.Verdict.DUE -> Unit
        }

        val accountId = inputs.accountId()
        val token = inputs.sessionToken()
        val identitySeed = inputs.accountIdentitySeed()
        // Leaves the dirty flag alone, so the backup goes out at the next tick
        // once the session is whole; clearing it here would silently drop the
        // change that made it dirty.
        if (accountId == null || token.isNullOrEmpty() || identitySeed == null) {
            return Outcome.NOT_READY
        }
        val snapshot = inputs.snapshot() ?: return Outcome.NOT_READY

        val seed = try {
            ed25519SeedFromIdentitySeed(identitySeed)
        } catch (cause: Throwable) {
            Log.w(TAG, "seed derive failed", cause)
            return Outcome.NOT_READY
        }

        // Read at upload time rather than watched: the list is small, the tick
        // is slow, and a snapshot taken now is by definition the one being
        // uploaded.
        val contacts = AcceptedContactsStore.load(applicationContext).map { c ->
            BackupContactRecord(
                ed25519Pubkey = c.request.fromPubkey,
                x25519Pubkey = ByteArray(32),
                displayName = c.displayName ?: "",
                accountId = c.request.fromAccountId,
            )
        }

        val client = BackupClient()
        client.setSessionToken(token)
        val result = try {
            client.uploadFromCache(
                context = applicationContext,
                accountId = accountId,
                accountIdB64 = accountIdB64,
                ed25519Seed = seed,
                contacts = contacts,
                wholeStorageSnapshot = snapshot,
                // The index the snapshot cannot supply. Read from the same
                // accessor the membership check above uses, so an upload
                // always carries the set it was marked dirty for.
                knownGroupIdsHex = groups ?: emptyList(),
            )
        } catch (cause: Throwable) {
            Log.w(TAG, "upload threw", cause)
            return Outcome.FAILED
        }
        if (result !is UploadResult.Uploaded) {
            // A cache miss is the ordinary state for a device that joined by
            // pairing rather than by signup or recover: it holds the account
            // but never saw the phrase, so it cannot seal an envelope. Not an
            // error, and not worth a line on every tick.
            if (result !is UploadResult.CacheMiss) {
                Log.w(TAG, "upload returned $result")
            }
            return Outcome.FAILED
        }
        AutoBackupStore.save(
            applicationContext,
            accountIdB64,
            AutoBackup.markedUploaded(state, nowMs),
        )
        return Outcome.UPLOADED
    }

    private companion object {
        const val TAG = "lakko-backup"
    }
}
