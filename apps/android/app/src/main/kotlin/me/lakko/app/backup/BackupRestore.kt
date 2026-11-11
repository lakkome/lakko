// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import android.content.Context
import android.content.SharedPreferences
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import uniffi.lakko_ffi_uniffi.BackupContactRecord
import uniffi.lakko_ffi_uniffi.BackupOwnPredecessor

/**
 * Restoring the account backup, on every route that ends with the recovery
 * phrase in hand (the restore screen, the phrase prompt after a sign-in on a
 * device with no cached seed).
 *
 * What a backup is (maintainer decision, 2026-10-02): the account's contacts,
 * the ADR-0058 synced state, the NAMES of its conversations, (ADR-0074
 * section 3.7) the account's own predecessor keys, and the own profile and
 * visibility envelopes. It carries no MLS state. A provider snapshot belongs to the device that made it: loaded on
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

    /** The fetch itself failed, or the own predecessors it carried could not
     *  be stored: no answer yet. The restore stays pending and the next
     *  session landing tries again. */
    FETCH_FAILED,
}

/** What storing a backup's own predecessors did. */
internal class OwnPredecessorsStored(
    /** The key this account's conversation leaves are bound to moved: a chat
     *  layer already loaded under another one is rebuilt. */
    val conversationKeyChanged: Boolean,
)

/** Everything [restoreBackupAndApply] touches, so a test can stand in for it. */
internal interface BackupRestorePorts {
    /** Download the blob, run the ADR-0037 head gate and open the envelope.
     *  [confirmStaleOverride] is asked on a stale head and must never
     *  auto-accept. */
    suspend fun fetch(confirmStaleOverride: suspend (RestoreReason) -> Boolean): GatedRestoreOutcome

    /** ADR-0074 section 3.7: store the account's own predecessors where a
     *  kept device holds them. `null` when they could not be stored. Called
     *  only with a non-empty list. Never log a seed. */
    fun storeOwnPredecessors(predecessors: List<BackupOwnPredecessor>): OwnPredecessorsStored?

    /** The path a device takes after a pairing bundle delivered predecessors:
     *  the binding is minted again under the oldest one while the device
     *  holds no conversation, and the KeyPackage published again. */
    suspend fun rebindConversationKey()

    /** Add the contacts this device does not hold yet. Never drops one it
     *  holds. Returns how many were added. */
    fun mergeContacts(contacts: List<BackupContactRecord>, createdAtMs: ULong): Int

    /** Fold the ADR-0058 synced state into what this device holds. */
    fun mergeSyncedState(bytes: ByteArray)

    /** Remember the conversations the backup named (see
     *  [RestoredKnownGroups]). */
    fun rememberRestoredGroups(groupIdsHex: List<String>)

    /** Keep the backup's own profile envelope: decode it fail closed (a throw
     *  changes nothing) and store the name and photo unless this device holds
     *  something newer. */
    fun applyOwnProfile(envelope: ByteArray)

    /** The backup's visibility envelope: decode it fail closed and hand it to
     *  the path an incoming DeviceGroup envelope takes. */
    fun applyVisibility(envelope: ByteArray)

    /** ADR-0048 / `lakko-backup` v8 — the received profiles, each through
     *  the profile store's receive gate. Returns how many were stored. */
    fun applyReceivedProfiles(entries: List<uniffi.lakko_ffi_uniffi.BackupReceivedProfile>): Int = 0

    /** What this device noted about the Everyone publication is out of date. */
    fun noteEveryoneUnknown()

    /** The restore finished: the chat list explains an empty list until the
     *  first conversation arrives (`docs/ux/identity-recovery.md` 7.2). */
    fun markRestored(knewConversations: Boolean)

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
 * Applies the account's own predecessors first (nothing below may leave the
 * device bound under the newest key), then contacts (merged), the synced state
 * (merged) and the known group ids. There is no MLS state in a backup and
 * nothing here can load any.
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
            runCatching { ports.markRestored(knewConversations = false) }
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

    // ADR-0074 section 3.7. On a phrase route this runs before the session
    // lands, so the session bootstrap mints the first binding under the
    // oldest key; on the retry the chat layer is rebuilt below.
    val predecessors = try {
        applyOwnPredecessors(route, ports, parsed.ownPredecessors)
    } finally {
        parsed.ownPredecessors.forEach { it.seed.fill(0) }
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
    // An empty field, or one that does not decode, changes nothing.
    try {
        ports.noteEveryoneUnknown()
        if (parsed.ownProfile.isNotEmpty()) ports.applyOwnProfile(parsed.ownProfile)
    } catch (cause: Throwable) {
        ports.log("backup restore ($route): own profile not restored", cause)
    }
    try {
        if (parsed.visibility.isNotEmpty()) ports.applyVisibility(parsed.visibility)
    } catch (cause: Throwable) {
        ports.log("backup restore ($route): visibility not restored", cause)
    }
    try {
        val names = ports.applyReceivedProfiles(parsed.receivedProfiles)
        ports.log("backup restore ($route): ${parsed.receivedProfiles.size} name(s) in the backup, $names stored")
    } catch (cause: Throwable) {
        ports.log("backup restore ($route): names not restored", cause)
    }
    try {
        ports.markRestored(knewConversations = parsed.knownGroupIds.isNotEmpty())
    } catch (cause: Throwable) {
        ports.log("backup restore ($route): restored marker not set", cause)
    }
    if (predecessors == OwnPredecessorStep.NOT_STORED) {
        // The next backup from this device would leave them out, and nothing
        // else on the server holds them: uploads stay held and the next
        // landing tries again.
        return BackupRestoreOutcome.FETCH_FAILED
    }
    try {
        ports.markBackupDirty()
    } catch (cause: Throwable) {
        ports.log("backup restore ($route): backup not marked dirty", cause)
    }
    if (predecessors == OwnPredecessorStep.KEY_MOVED) {
        try {
            ports.rebindConversationKey()
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            // The next bootstrap reads the predecessor store itself.
            ports.log("backup restore ($route): conversation identity not rebuilt", cause)
        }
    }
    ports.setRetryPending(false)
    return BackupRestoreOutcome.APPLIED
}

private enum class OwnPredecessorStep { NOTHING, STORED, KEY_MOVED, NOT_STORED }

private fun applyOwnPredecessors(
    route: String,
    ports: BackupRestorePorts,
    predecessors: List<BackupOwnPredecessor>,
): OwnPredecessorStep {
    if (predecessors.isEmpty()) return OwnPredecessorStep.NOTHING
    val stored = try {
        ports.storeOwnPredecessors(predecessors)
    } catch (cause: Throwable) {
        ports.log("backup restore ($route): own predecessors not stored", cause)
        null
    }
    return when {
        stored == null -> OwnPredecessorStep.NOT_STORED
        stored.conversationKeyChanged -> OwnPredecessorStep.KEY_MOVED
        else -> OwnPredecessorStep.STORED
    }.also { ports.log("backup restore ($route): ${predecessors.size} own predecessor(s), $it") }
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
    private const val RESTORED_CHATS_PREFIX = "restored-chats:"
    private const val REJOIN_ANSWER_PREFIX = "restored-rejoin:"

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
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            val current = restoredGroups(context, accountIdB64)
            val id = groupIdHex.lowercase()
            if (id !in current) return
            saveGroups(context, accountIdB64, current - id)
        }
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

    /** Record that a restore finished on this device (the phrase or a
     *  passkey). Read by the chat list ([restoredChatsHint]). */
    public fun markRestoredChats(context: Context, accountIdB64: String, knewConversations: Boolean) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            prefs(context).edit().putBoolean(RESTORED_CHATS_PREFIX + accountIdB64, knewConversations).apply()
        }
    }

    /** `null` when no restore is waiting for its first conversation, else
     *  whether the backup named at least one. */
    public fun restoredChats(context: Context, accountIdB64: String): Boolean? {
        val key = RESTORED_CHATS_PREFIX + accountIdB64
        val held = prefs(context)
        return if (held.contains(key)) held.getBoolean(key, false) else null
    }

    /** The first conversation arrived: the hint is gone for good. */
    public fun clearRestoredChats(context: Context, accountIdB64: String) {
        prefs(context).edit()
            .remove(RESTORED_CHATS_PREFIX + accountIdB64)
            .remove(REJOIN_ANSWER_PREFIX + accountIdB64)
            .apply()
    }

    /**
     * Record what this device's DeviceGroup rejoin came to (ADR-0081 third
     * amendment): another device answered its knock, or it founded the
     * DeviceGroup itself. The latest decision wins. Read by the chat list
     * ([restoredChatsHint]), so a restart shows the same state.
     */
    public fun recordRejoinAnswer(context: Context, accountIdB64: String, answer: RestoreRejoinAnswer) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            prefs(context).edit().putString(REJOIN_ANSWER_PREFIX + accountIdB64, answer.stored).apply()
        }
    }

    /** What the rejoin came to, or `null` while it has not decided. */
    public fun rejoinAnswer(context: Context, accountIdB64: String): RestoreRejoinAnswer? =
        RestoreRejoinAnswer.fromStored(prefs(context).getString(REJOIN_ANSWER_PREFIX + accountIdB64, null))

    /** [rejoinAnswer], and every change of it after. */
    public fun rejoinAnswerFlow(
        context: Context,
        accountIdB64: String,
    ): Flow<RestoreRejoinAnswer?> {
        val key = REJOIN_ANSWER_PREFIX + accountIdB64
        val held = prefs(context)
        return callbackFlow {
            val listener = SharedPreferences.OnSharedPreferenceChangeListener { _, changed ->
                if (changed == null || changed == key) {
                    trySend(RestoreRejoinAnswer.fromStored(held.getString(key, null)))
                }
            }
            held.registerOnSharedPreferenceChangeListener(listener)
            trySend(RestoreRejoinAnswer.fromStored(held.getString(key, null)))
            awaitClose { held.unregisterOnSharedPreferenceChangeListener(listener) }
        }.distinctUntilChanged()
    }

    /** Forget device / Delete account: everything of [accountIdB64]. */
    public fun clear(context: Context, accountIdB64: String) {
        prefs(context).edit()
            .remove(GROUPS_PREFIX + accountIdB64)
            .remove(RETRY_PREFIX + accountIdB64)
            .remove(RESTORED_CHATS_PREFIX + accountIdB64)
            .remove(REJOIN_ANSWER_PREFIX + accountIdB64)
            .apply()
    }
}

/** What this device's DeviceGroup rejoin came to after a restore
 *  (`docs/ux/identity-recovery.md` 7.2, amended 2026-11-11). */
public enum class RestoreRejoinAnswer(internal val stored: String) {
    /** Another device of the account answered the knock (a presence note
     *  or a Welcome inside the answer window). */
    ANSWERED("answered"),

    /** Nobody answered, or no other device was listed: this device founded
     *  the DeviceGroup. */
    FOUNDED("founded"),
    ;

    public companion object {
        /** `null` for anything this build did not write. */
        public fun fromStored(value: String?): RestoreRejoinAnswer? = entries.firstOrNull { it.stored == value }
    }
}

/** What the chat list says after a restore (`docs/ux/identity-recovery.md` 7.2). */
public enum class RestoredChatsHint {
    /** The ordinary list. */
    NONE,

    /** The card on the empty list, with a button to Contacts. */
    CARD,
}

/**
 * `docs/ux/identity-recovery.md` 7.2 (amended 2026-11-11). Whether another
 * device is merely listed in the directory says nothing (a lost device stays
 * listed); only an answer to this device's rejoin knock does. A device that
 * founded the DeviceGroup because nobody answered shows the card; one that
 * another device answered shows the ordinary list (that device adds it to
 * the chats in the background, nothing the person can act on).
 *
 * @param knewConversations [BackupRestoreStore.restoredChats]: `null` when no
 *        restore is waiting for its first conversation, else whether the
 *        backup named at least one conversation.
 * @param listEmpty no conversation on this device yet.
 * @param rejoin [BackupRestoreStore.rejoinAnswer]: `null` until the rejoin
 *        decided.
 */
public fun restoredChatsHint(
    knewConversations: Boolean?,
    listEmpty: Boolean,
    rejoin: RestoreRejoinAnswer?,
): RestoredChatsHint = when {
    knewConversations == true && listEmpty && rejoin == RestoreRejoinAnswer.FOUNDED -> RestoredChatsHint.CARD
    else -> RestoredChatsHint.NONE
}
