// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import android.content.Context
import android.content.SharedPreferences
import kotlinx.coroutines.CancellationException
import uniffi.lakko_ffi_uniffi.BackupContactRecord

/**
 * Restoring the account backup, on every route that ends with the recovery
 * phrase in hand (the restore screen, the phrase prompt after a sign-in on a
 * device with no cached seed).
 *
 * What a backup is (maintainer decision, 2026-10-02): the account's contacts,
 * the ADR-0058 synced state and the NAMES of its conversations. It carries no
 * MLS state. A provider snapshot belongs to the device that made it: loaded on
 * another device it clones a leaf (ADR-0050), loaded on the same device after
 * a reinstall it rolls the device back behind commits it already published
 * (ADR-0040) and re-uses ratchet positions (G-34). A restored device keeps
 * whatever MLS state it already held locally for the account, possibly none,
 * and is admitted to its chats by the other members (ADR-0050 section 2.5a).
 *
 * The rules live here behind [BackupRestorePorts] so they run on the host JVM;
 * `AuthRepository` supplies the ports.
 */

/** How one run of [restoreBackupAndApply] ended. Never a failure of the
 *  sign-in it ran inside. */
public enum class BackupRestoreOutcome {
    /** A backup was fetched, passed the ADR-0037 gate, opened and applied. */
    APPLIED,

    /** The server holds no backup for the account (404). Not an error. */
    NO_BACKUP,

    /** A backup was served and not applied: the gate refused it, the person
     *  declined a stale head, or it did not open under the phrase. */
    NOT_APPLIED,

    /** The fetch itself failed: no answer yet. The restore stays pending and
     *  the next session landing tries again. */
    FETCH_FAILED,
}

/** Everything [restoreBackupAndApply] touches, so a test can stand in for it. */
internal interface BackupRestorePorts {
    /** Download the blob, run the ADR-0037 head gate and open the envelope.
     *  [confirmStaleOverride] is asked on a stale head and must never
     *  auto-accept. */
    suspend fun fetch(confirmStaleOverride: suspend (RestoreReason) -> Boolean): GatedRestoreOutcome

    /** Add the contacts this device does not hold yet. Never drops one it
     *  holds. Returns how many were added. */
    fun mergeContacts(contacts: List<BackupContactRecord>, createdAtMs: ULong): Int

    /** Fold the ADR-0058 synced state into what this device holds. */
    fun mergeSyncedState(bytes: ByteArray)

    /** Remember the conversations the backup named (see
     *  [RestoredKnownGroups]). */
    fun rememberRestoredGroups(groupIdsHex: List<String>)

    /** The merged state is worth uploading again. */
    fun markBackupDirty()

    /** Set or clear the "this account's backup restore is pending" flag:
     *  set before the fetch starts, cleared only by a definitive answer. */
    fun setRetryPending(pending: Boolean)

    /** Whether that flag is set. */
    fun retryPending(): Boolean

    /** The gate refused the blob outright (tamper or corruption). */
    fun raiseRollbackAlert(reason: RestoreReason)

    fun log(message: String, cause: Throwable? = null)
}

/**
 * THE restore: download, gate, open, apply. One function for every route.
 *
 * Applies contacts (merged), the synced state (merged) and the known group
 * ids. There is no MLS state in a backup and nothing here can load any.
 *
 * Never throws (cancellation aside) and never fails the sign-in around it: a
 * missing backup is [BackupRestoreOutcome.NO_BACKUP]; a failed fetch is
 * [BackupRestoreOutcome.FETCH_FAILED].
 *
 * The "restore is pending" flag is set BEFORE the fetch starts and cleared
 * only by a DEFINITIVE answer: applied, no backup, refused by the head gate, a
 * stale head that was declined, or a payload that does not open under the
 * phrase. A fetch failure is not an answer, and neither is a process killed
 * mid-restore: the flag stays, [retryPendingBackupRestore] tries again at the
 * next session landing, and this device's own uploads are held meanwhile (an
 * upload made before the restore ran would replace the very backup it still
 * has to read).
 *
 * @param route which route ran it, for the log only.
 * @param confirmStaleOverride the route's answer to a stale head: the restore
 *        screen's dialog where there is one, `{ false }` where there is none.
 */
internal suspend fun restoreBackupAndApply(
    route: String,
    ports: BackupRestorePorts,
    confirmStaleOverride: suspend (RestoreReason) -> Boolean,
): BackupRestoreOutcome {
    // Before anything else, so a process that dies from here on is retried.
    ports.setRetryPending(true)
    val fetched = try {
        ports.fetch(confirmStaleOverride)
    } catch (cancel: CancellationException) {
        throw cancel
    } catch (cause: Throwable) {
        GatedRestoreOutcome.TransportFailed(cause)
    }
    val parsed = when (fetched) {
        is GatedRestoreOutcome.Restored -> fetched.payload
        is GatedRestoreOutcome.NoBackup -> {
            ports.log("backup restore ($route): no backup on the server")
            ports.setRetryPending(false)
            return BackupRestoreOutcome.NO_BACKUP
        }
        is GatedRestoreOutcome.WrongPhraseOrTampered -> {
            ports.log("backup restore ($route): not applied, wrong phrase or tampered")
            ports.setRetryPending(false)
            return BackupRestoreOutcome.NOT_APPLIED
        }
        is GatedRestoreOutcome.RollbackBlocked -> {
            if (fetched.hard) {
                ports.log("backup restore ($route): ROLLBACK BLOCKED (hard) - ${fetched.reason.diagnostic}")
                ports.raiseRollbackAlert(fetched.reason)
            } else {
                ports.log("backup restore ($route): stale head not applied - ${fetched.reason.diagnostic}")
            }
            ports.setRetryPending(false)
            return BackupRestoreOutcome.NOT_APPLIED
        }
        is GatedRestoreOutcome.TransportFailed -> {
            // Not an answer: the flag set above stays.
            ports.log("backup restore ($route): fetch failed, will retry at the next landing", fetched.cause)
            return BackupRestoreOutcome.FETCH_FAILED
        }
    }

    // Each part stands alone: one that fails must not cost the others.
    try {
        val added = ports.mergeContacts(parsed.acceptedContacts, parsed.createdAtMs)
        ports.log(
            "backup restore ($route): ${parsed.acceptedContacts.size} contact(s) in the backup, $added added",
        )
    } catch (cause: Throwable) {
        ports.log("backup restore ($route): contact merge failed", cause)
    }
    try {
        if (parsed.syncedState.isNotEmpty()) ports.mergeSyncedState(parsed.syncedState)
    } catch (cause: Throwable) {
        ports.log("backup restore ($route): synced state not merged", cause)
    }
    try {
        val groupIdsHex = parsed.knownGroupIds.map(::hexOf)
        ports.rememberRestoredGroups(groupIdsHex)
        ports.log("backup restore ($route): ${groupIdsHex.size} known group(s)")
    } catch (cause: Throwable) {
        ports.log("backup restore ($route): known groups not recorded", cause)
    }
    try {
        ports.markBackupDirty()
    } catch (cause: Throwable) {
        ports.log("backup restore ($route): backup not marked dirty", cause)
    }
    ports.setRetryPending(false)
    return BackupRestoreOutcome.APPLIED
}

/**
 * A restore still pending from an earlier landing (its fetch failed, or the
 * process died mid-restore), run at a session landing: ONE attempt per
 * landing. The flag clears only on a definitive answer, so an attempt whose
 * fetch fails again stays pending - uploads stay held - and the landing after
 * tries again.
 *
 * There is no screen on this path, so a stale head is "do not apply": it is
 * logged and declined, which is a definitive answer.
 *
 * Returns `null` when nothing was pending.
 */
internal suspend fun retryPendingBackupRestore(ports: BackupRestorePorts): BackupRestoreOutcome? {
    if (!ports.retryPending()) return null
    return restoreBackupAndApply(
        route = "retry",
        ports = ports,
        confirmStaleOverride = { reason ->
            ports.log("backup restore (retry): stale head, no screen to ask on - ${reason.diagnostic}")
            false
        },
    )
}

private fun hexOf(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it) }

/**
 * A backup always lists the account's conversations.
 *
 * A restored device holds none of the groups its backup named until other
 * members admit it, so "the groups this device holds" would be an empty list
 * and the first upload after a restore would erase the names. The ids a
 * restored backup carried are therefore kept ("restored known groups") and
 * every later upload carries the union of the two.
 *
 * An id leaves the restored set when the device joins that group, when the
 * person deletes that chat, or when the local group has ended (the device was
 * removed or left) - from then on the device's own list speaks for it.
 *
 * This is NOT the pairing bundle's pending-known-groups list
 * (`devices.StateBundle`) and never feeds the "Synchronizing..." label.
 */
public object RestoredKnownGroups {

    /** The group ids an upload carries: [heldIdsHex] (what this device holds),
     *  then the [restoredIdsHex] it has not joined. No duplicates. */
    public fun backupGroupIds(
        heldIdsHex: Collection<String>,
        restoredIdsHex: Collection<String>,
    ): List<String> {
        val seen = LinkedHashSet<String>()
        for (id in heldIdsHex) seen.add(id.lowercase())
        for (id in restoredIdsHex) seen.add(id.lowercase())
        return seen.toList()
    }

    /** The restored set once a backup naming [namedIdsHex] is applied on a
     *  device holding [heldIdsHex]: additive, minus what it already holds. */
    public fun afterRestore(
        current: Set<String>,
        namedIdsHex: Collection<String>,
        heldIdsHex: Collection<String>,
    ): Set<String> {
        val held = heldIdsHex.mapTo(HashSet()) { it.lowercase() }
        val next = LinkedHashSet(current)
        for (id in namedIdsHex) next.add(id.lowercase())
        next.removeAll(held)
        return next
    }
}

/**
 * Per-account state of the backup restore: the restored known groups and the
 * "restore is pending" flag. Plain `SharedPreferences` - group ids and one boolean, nothing a
 * server does not already see in the journal names.
 *
 * Wiped with the account's local data (Forget device / Delete account); kept
 * on Log out for the account's next sign-in, like the other per-account
 * stores.
 */
public object BackupRestoreStore {

    private const val PREFS = "lakko.backup.restore"
    private const val GROUPS_PREFIX = "restored-groups:"
    private const val RETRY_PREFIX = "retry:"

    private fun prefs(context: Context): SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    /** The group ids restored backups named that this device has not joined. */
    public fun restoredGroups(context: Context, accountIdB64: String): Set<String> =
        prefs(context).getStringSet(GROUPS_PREFIX + accountIdB64, emptySet())?.toSet() ?: emptySet()

    /** Record the ids a restored backup named. Additive; ids in [heldIdsHex]
     *  (groups this device already holds) are not recorded. */
    public fun addRestoredGroups(
        context: Context,
        accountIdB64: String,
        namedIdsHex: Collection<String>,
        heldIdsHex: Collection<String>,
    ) {
        val current = restoredGroups(context, accountIdB64)
        saveGroups(context, accountIdB64, RestoredKnownGroups.afterRestore(current, namedIdsHex, heldIdsHex))
    }

    /** Drop one id: the device joined the group, the chat was deleted, or the
     *  local group ended. No-op when absent. */
    public fun removeRestoredGroup(context: Context, accountIdB64: String, groupIdHex: String) {
        val current = restoredGroups(context, accountIdB64)
        val id = groupIdHex.lowercase()
        if (id !in current) return
        saveGroups(context, accountIdB64, current - id)
    }

    /**
     * The group ids an upload from this device carries for [heldIdsHex], the
     * groups it holds. Drops the held ones from the restored set on the way,
     * so a group joined by a route that did not say so stops being listed
     * twice over.
     */
    public fun backupGroupIds(
        context: Context,
        accountIdB64: String,
        heldIdsHex: Collection<String>,
    ): List<String> {
        val current = restoredGroups(context, accountIdB64)
        val remaining = RestoredKnownGroups.afterRestore(current, emptyList(), heldIdsHex)
        if (remaining != current) saveGroups(context, accountIdB64, remaining)
        return RestoredKnownGroups.backupGroupIds(heldIdsHex, remaining)
    }

    private fun saveGroups(context: Context, accountIdB64: String, ids: Set<String>) {
        val editor = prefs(context).edit()
        if (ids.isEmpty()) {
            editor.remove(GROUPS_PREFIX + accountIdB64)
        } else {
            editor.putStringSet(GROUPS_PREFIX + accountIdB64, ids)
        }
        editor.apply()
    }

    /** Whether the account's backup restore is still pending: started on a
     *  phrase route and not yet answered definitively. While it is, this
     *  device uploads no backup and each session landing tries once more. */
    public fun retryPending(context: Context, accountIdB64: String): Boolean =
        prefs(context).getBoolean(RETRY_PREFIX + accountIdB64, false)

    public fun setRetryPending(context: Context, accountIdB64: String, pending: Boolean) {
        val editor = prefs(context).edit()
        if (pending) {
            editor.putBoolean(RETRY_PREFIX + accountIdB64, true)
        } else {
            editor.remove(RETRY_PREFIX + accountIdB64)
        }
        editor.apply()
    }

    /** Forget device / Delete account: everything of [accountIdB64]. */
    public fun clear(context: Context, accountIdB64: String) {
        prefs(context).edit()
            .remove(GROUPS_PREFIX + accountIdB64)
            .remove(RETRY_PREFIX + accountIdB64)
            .apply()
    }
}
