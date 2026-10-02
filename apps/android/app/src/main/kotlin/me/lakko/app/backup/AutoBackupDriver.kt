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
 * asked HERE, at the tick, rather than hooked onto the MLS state write: that
 * path runs only after an MLS mutation, so a device that had already joined
 * everything and merely opened the app never marked itself dirty - precisely
 * the person whose backup is most stale. Asking at the tick also makes the
 * first run after install a change, because nothing on this device can vouch
 * for what the server already holds.
 *
 * What goes up: contacts, the ADR-0058 synced state and the ids of the
 * account's conversations. No MLS provider state - a backup carries none
 * (see `BackupRestore.kt`).
 */
public class AutoBackupDriver internal constructor(
    private val applicationContext: Context,
    private val scope: CoroutineScope,
    private val inputs: Inputs,
    private val uploader: Uploader,
) {

    public constructor(
        applicationContext: Context,
        scope: CoroutineScope,
        inputs: Inputs,
    ) : this(applicationContext, scope, inputs, CachedPhraseUploader)

    /** One upload, as the driver asks for it. Production seals with the cached
     *  phrase and PUTs ([CachedPhraseUploader]); a test records the request. */
    internal fun interface Uploader {
        suspend fun upload(request: UploadRequest): UploadResult
    }

    /** What one upload carries. */
    internal class UploadRequest(
        val context: Context,
        val accountId: ByteArray,
        val accountIdB64: String,
        val sessionToken: String,
        val accountIdentitySeed: ByteArray,
        val contacts: List<BackupContactRecord>,
        /** The held groups plus the restored ones not joined yet. */
        val knownGroupIdsHex: List<String>,
        val syncedState: ByteArray?,
    )

    /**
     * Everything a run needs from the session, read at each tick because a
     * session token is rebound and the group set moves.
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
        /** ADR-0058 — `SyncedState.toBytes()`, or `null` when the sync
         *  runtime holds none; carried in the backup so a restore with no
         *  other device alive gets the folders and flags back. */
        val syncedState: () -> ByteArray? = { null },
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

        // The ids an upload would carry: what this device holds plus what a
        // restored backup named and it has not joined yet. The change check
        // runs over that union, so a restored group being joined - the same
        // set, one id moving from one half to the other - is not a change.
        val held = inputs.knownGroupIds()
        val groups = held?.let {
            BackupRestoreStore.backupGroupIds(applicationContext, accountIdB64, it)
        }
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
        // A backup always lists the conversations, so one composed before the
        // chat layer has loaded them would upload "none" and erase the list.
        if (groups == null) return Outcome.NOT_READY
        // The account's backup restore has not reached a definitive answer
        // yet (its fetch failed, or it is running): an upload now would
        // replace the very backup that restore still has to read.
        if (BackupRestoreStore.retryPending(applicationContext, accountIdB64)) {
            return Outcome.NOT_READY
        }

        // Read at upload time rather than watched: the list is small and the
        // tick is slow.
        // This account's list, whatever is bound when the tick runs.
        val contacts = AcceptedContactsStore.load(applicationContext, accountIdB64).map { c ->
            BackupContactRecord(
                ed25519Pubkey = c.request.fromPubkey,
                x25519Pubkey = ByteArray(32),
                displayName = c.displayName ?: "",
                accountId = c.request.fromAccountId,
            )
        }

        val result = try {
            uploader.upload(
                UploadRequest(
                    context = applicationContext,
                    accountId = accountId,
                    accountIdB64 = accountIdB64,
                    sessionToken = token,
                    accountIdentitySeed = identitySeed,
                    contacts = contacts,
                    // The same set the membership check above ran over, so an
                    // upload always carries the set it was marked dirty for.
                    knownGroupIdsHex = groups,
                    syncedState = inputs.syncedState(),
                ),
            )
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
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

/**
 * The production [AutoBackupDriver.Uploader]: derive the backup signing seed,
 * seal under the phrase this device cached and PUT. A device that never saw
 * the phrase answers [UploadResult.CacheMiss].
 */
internal object CachedPhraseUploader : AutoBackupDriver.Uploader {
    override suspend fun upload(request: AutoBackupDriver.UploadRequest): UploadResult {
        val seed = try {
            ed25519SeedFromIdentitySeed(request.accountIdentitySeed)
        } catch (cause: Throwable) {
            return UploadResult.TransportFailed(cause)
        }
        val client = BackupClient()
        try {
            client.setSessionToken(request.sessionToken)
            return client.uploadFromCache(
                context = request.context,
                accountId = request.accountId,
                accountIdB64 = request.accountIdB64,
                ed25519Seed = seed,
                contacts = request.contacts,
                knownGroupIdsHex = request.knownGroupIdsHex,
                syncedState = request.syncedState,
            )
        } finally {
            client.close()
            seed.fill(0)
        }
    }
}
