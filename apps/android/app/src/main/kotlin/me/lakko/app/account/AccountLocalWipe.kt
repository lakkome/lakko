// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.account

import android.content.Context
import android.util.Log
import java.io.File

/**
 * Forget device and Delete account leave nothing of the account behind
 * (maintainer rule, 2026-09-30).
 *
 * [wipeAccountLocalData] is the ONE list of this client's per-account
 * persistent stores. Every store keyed by the account (or holding rows tagged
 * with it) is removed for that account, and every retired device-wide row a
 * store no longer reads is removed with it, since it may still hold this
 * account's data. Other accounts' rows stay, and so do genuine device
 * settings: the theme (`lakko-appearance`), the media preferences
 * (`lakko-media-prefs`: record mode, autoplay, the GIF note, recent emoji and
 * tones), the KT-log root floor (`lakko-transparency-root`), the
 * notification-permission prompt marker, the anti-censorship and release
 * checker preferences, the push device pseudonym (`lakko-push`, handled by the
 * push unsubscribe) and the Keystore wrap keys shared by every account.
 *
 * Log out does NOT come here: it keeps the account's data for its next
 * sign-in.
 *
 * Order: the push wake binding and this app's delivered notifications go
 * first, then every other store. Each store is its own best-effort step: one
 * that fails (a Keystore the OS invalidated, say) is logged and never stops
 * the others. The caller has already stopped the in-memory runtimes that
 * could write a store back (chat, contacts, channels, device sync), so a
 * deleted row stays deleted.
 *
 * Called from exactly one place, [me.lakko.app.auth.AuthRepository.forgetDevice],
 * which Forget device and Delete account both reach through the same
 * orchestration in `MainActivity`.
 */
public object AccountLocalWipe {

    private const val TAG: String = "lakko-wipe"

    /** One named best-effort deletion. */
    public class Step internal constructor(public val name: String, internal val run: (Context, String) -> Unit)

    /**
     * Remove every piece of [accountIdB64]'s local data on this device.
     * Never throws. Returns the names of the steps that failed (empty when
     * every store was removed); each failure is also logged.
     */
    public fun wipeAccountLocalData(context: Context, accountIdB64: String): List<String> =
        runSteps(context, accountIdB64, steps)

    /** [wipeAccountLocalData] over an explicit step list (tests prove the
     *  inventory walk notices a store left out). */
    internal fun runSteps(context: Context, accountIdB64: String, steps: List<Step>): List<String> {
        if (accountIdB64.isEmpty()) return emptyList()
        val app = context.applicationContext
        val failed = ArrayList<String>()
        for (step in steps) {
            try {
                step.run(app, accountIdB64)
            } catch (cause: Throwable) {
                failed += step.name
                Log.w(TAG, "wipe step ${step.name} failed: ${cause.javaClass.simpleName}")
            }
        }
        if (failed.isEmpty()) {
            Log.i(TAG, "account data wiped (${steps.size} stores)")
        } else {
            Log.w(TAG, "account data wiped with ${failed.size} of ${steps.size} stores failing")
        }
        return failed
    }

    /** The stores, in the order they are removed. */
    public val steps: List<Step> = listOf(
        // ---- push first (ADR-0042, amended 2026-09-30) ----------------------
        Step("push-sign-out") { ctx, _ -> me.lakko.app.push.PushSignOut.clearFirst(ctx) },
        Step("push-routing-secrets") { ctx, acct ->
            me.lakko.app.push.PushRoutingSecretStore.clearForAccount(ctx, acct)
        },
        Step("push-route-index") { ctx, acct -> me.lakko.app.push.PushRouteStore(ctx).clearForAccount(acct) },

        // ---- session + identity material ------------------------------------
        Step("session-token") { ctx, acct -> me.lakko.app.auth.SessionTokenStore.deleteSessionToken(ctx, acct) },
        Step("session-record") { ctx, acct ->
            val store = me.lakko.app.auth.SessionStore(ctx)
            if (store.load()?.accountIdB64 == acct) store.clear()
        },
        Step("device-key") { ctx, acct -> me.lakko.app.auth.DeviceKeyStore.deleteDeviceKey(ctx, acct) },
        Step("account-identity-seed") { ctx, acct ->
            me.lakko.app.auth.AccountIdentitySeedStore.deleteAccountIdentitySeed(ctx, acct)
        },
        Step("device-identity-seed") { ctx, acct ->
            me.lakko.app.auth.DeviceIdentityStore.deleteDeviceIdentitySeed(ctx, acct)
        },
        Step("export-key") { ctx, acct -> me.lakko.app.auth.ExportKeyStore.deleteExportKey(ctx, acct) },
        // One pool per device, owned by the session that filled it (the one
        // ending now): the in-memory pool and its file both go.
        Step("anon-token-pool") { ctx, _ ->
            me.lakko.app.anon.AnonTokens.clear()
            me.lakko.app.anon.FileAnonTokenStore.under(ctx.noBackupFilesDir).clear()
        },

        // ---- backup ---------------------------------------------------------
        Step("recovery-phrase") { ctx, acct -> me.lakko.app.backup.PhraseStore.deletePhrase(ctx, acct) },
        Step("backup-floor") { ctx, acct -> me.lakko.app.backup.FloorStore.deleteFloor(ctx, acct) },
        Step("auto-backup") { ctx, acct -> me.lakko.app.backup.AutoBackupStore.clear(ctx, acct) },

        // ---- DeviceGroup + multi-device --------------------------------------
        Step("device-group-snapshot") { ctx, acct ->
            me.lakko.app.devices.DeviceGroupSnapshotStore.deleteSnapshot(ctx, acct)
        },
        Step("device-sync-state") { ctx, acct ->
            me.lakko.app.devices.sync.DeviceGroupSyncRepository.deleteAccountRows(ctx, acct)
        },
        Step("device-group-founding") { ctx, acct ->
            me.lakko.app.devices.DeviceGroupJournalAccess.clearFoundingUnconfirmed(ctx, acct)
        },
        Step("pairing-state-bundle") { ctx, acct -> me.lakko.app.devices.StateBundle.forgetAccount(ctx, acct) },
        Step("device-link-answers") { ctx, acct -> me.lakko.app.devices.forgetDeviceLinkAnswers(ctx, acct) },
        Step("device-readmission") { ctx, acct -> me.lakko.app.chat.DeviceReadmission.clearForAccount(ctx, acct) },

        // ---- chat -----------------------------------------------------------
        Step("mls-snapshot") { ctx, acct -> me.lakko.app.chat.IdentitySnapshotStore.deleteSnapshot(ctx, acct) },
        Step("message-log") { ctx, acct -> me.lakko.app.chat.MessageLogStore.deleteLog(ctx, acct) },
        Step("known-groups") { ctx, acct -> me.lakko.app.chat.KnownGroupsStore(ctx).clear(acct) },
        Step("commit-outbox") { ctx, acct -> me.lakko.app.chat.CommitOutboxStore.deleteForAccount(ctx, acct) },
        Step("committer-beacons") { ctx, acct ->
            me.lakko.app.chat.SharedPrefsBeaconObservationStore(ctx).clearForAccount(acct)
        },
        Step("pending-requests") { ctx, acct ->
            me.lakko.app.chat.SharedPrefsPendingRequestStore(ctx).clearForAccount(acct)
        },
        Step("unapplied-requests") { ctx, acct ->
            me.lakko.app.chat.SharedPrefsUnappliedRequestStore(ctx).clearForAccount(acct)
        },
        Step("last-observed-owner") { ctx, acct ->
            me.lakko.app.chat.SharedPrefsLastObservedOwnerStore(ctx).clearForAccount(acct)
        },
        Step("leave-records") { ctx, acct -> me.lakko.app.chat.SharedPrefsLeaveRecordStore(ctx).clearForAccount(acct) },
        Step("journal-ownership") { ctx, acct ->
            me.lakko.app.ds.EncryptedJournalOwnershipStore(ctx).clearForAccount(acct)
        },
        Step("group-names") { ctx, acct -> me.lakko.app.chat.GroupNameStore.deleteAccount(ctx, acct) },
        Step("pins") { ctx, acct -> me.lakko.app.chat.PinStore.deleteAccount(ctx, acct) },
        Step("read-watermarks") { ctx, acct ->
            me.lakko.app.chat.SharedPreferencesReadWatermarkStore(ctx).deleteAccount(acct)
        },
        Step("conversation-kinds") { ctx, acct -> me.lakko.app.chat.ConversationKindStore.deleteAccount(ctx, acct) },
        Step("group-presentation") { ctx, acct ->
            me.lakko.app.ui.chat.GroupPresentationStore.deleteAccount(ctx, acct)
        },
        Step("folder-tab") { ctx, acct -> me.lakko.app.ui.chat.FolderTabPrefs.deleteAccount(ctx, acct) },
        Step("played-media-marks") { ctx, acct -> me.lakko.app.media.MediaPrefs.forgetAccountOnDevice(ctx, acct) },

        // ---- calls ----------------------------------------------------------
        Step("group-calls") { ctx, acct ->
            me.lakko.app.calls.SharedPreferencesGroupCallStore(ctx).deleteAccount(acct)
        },
        Step("call-history") { ctx, acct -> me.lakko.app.calls.CallHistoryStore.clear(ctx, acct) },

        // ---- profiles + reach -----------------------------------------------
        Step("peer-profiles") { ctx, acct ->
            me.lakko.app.profile.ProfileDirectory.forgetAccountOnDevice(ctx, acct)
        },
        Step("own-profile") { ctx, acct ->
            me.lakko.app.profile.OwnProfileStore.delete(ctx, me.lakko.app.auth.base64Decode(canonical(acct)))
        },
        Step("profile-requests") { ctx, acct ->
            me.lakko.app.profile.ProfileRequestPrefsStore(ctx).deleteAccount(acct)
        },
        Step("reach") { ctx, acct -> me.lakko.app.reach.EncryptedReachDocStore(ctx).delete(acct) },

        // ---- contacts -------------------------------------------------------
        Step("accepted-contacts") { ctx, acct -> me.lakko.app.contacts.AcceptedContactsStore.delete(ctx, acct) },
        Step("contacts-inbox") { ctx, acct ->
            me.lakko.app.contacts.SharedPreferencesContactsInboxStore(ctx).delete(acct)
        },
        Step("identity-pins") { ctx, acct -> me.lakko.app.contacts.PeerPinStore.deleteForOwner(ctx, acct) },
        Step("invite-link-secrets") { ctx, acct ->
            me.lakko.app.contacts.EncryptedInviteLinkSecretStore(ctx).clearForAccount(acct)
        },

        // ---- channels -------------------------------------------------------
        Step("channel-co-published") { ctx, acct ->
            me.lakko.app.channels.CoPublishedJournals.onDevice(ctx) { null }.deleteAccount(acct)
        },
        Step("channel-publisher-snapshot") { ctx, acct ->
            me.lakko.app.channels.ChannelPublisherIdentity.deleteOnDeviceSnapshot(ctx, acct)
        },
        Step("channel-log-and-keys") { ctx, acct ->
            val subscriberPrefs = me.lakko.app.channels.ChannelKeyStore.encryptedPrefs(ctx)
            me.lakko.app.channels.ChannelsUiState.deleteAccountRows(
                prefs = subscriberPrefs,
                publisherPrefs = null,
                logStore = me.lakko.app.channels.SharedPreferencesChannelLogStore(subscriberPrefs),
                accountIdB64 = acct,
            )
        },
        Step("channel-publisher-keys") { ctx, acct ->
            me.lakko.app.channels.ChannelsUiState.deleteAccountRows(
                prefs = null,
                publisherPrefs = me.lakko.app.channels.ChannelPublisherKeyStore.encryptedPrefs(ctx),
                logStore = null,
                accountIdB64 = acct,
            )
        },

        // ---- plaintext caches -----------------------------------------------
        // Device-wide directories, not keyed by account, that hold decrypted
        // media of the session that is ending: an open chat's media, finished
        // recordings waiting for upload, and attachments handed to another app.
        Step("media-caches") { ctx, _ ->
            for (dir in listOf(
                me.lakko.app.media.ChatMediaCache.ROOT_DIR,
                me.lakko.app.ui.chat.ChatViewModel.MEDIA_OUTBOX_DIR,
                me.lakko.app.ui.chat.SHARED_ATTACHMENTS_DIR,
            )) {
                File(ctx.cacheDir, dir).deleteRecursively()
            }
        },

        // ---- the backstop ---------------------------------------------------
        // Walk the whole no-backup directory and delete any file named by the
        // account: a temp file a crash left beside an envelope, a leaf-key
        // sidecar, or a store added later whose deletion was not listed above.
        Step("no-backup-files-sweep") { ctx, acct -> sweepFilesNamedBy(ctx.noBackupFilesDir, acct) },
    )

    /** The spellings of [accountIdB64] a file name can carry. */
    internal fun fileNameStems(accountIdB64: String): Set<String> {
        val std = canonical(accountIdB64)
        return setOf(
            // The Keystore envelope stores: standard base64 with '/' -> '_'.
            std.replace('/', '_'),
            accountIdB64.replace('/', '_'),
            // The played-marks store: URL-safe base64 without padding.
            std.replace('+', '-').replace('/', '_').trimEnd('='),
        )
    }

    /** Delete every file under [root] whose name is one of the account's
     *  stems, or a stem followed by `.`. */
    internal fun sweepFilesNamedBy(root: File, accountIdB64: String) {
        val stems = fileNameStems(accountIdB64)
        root.walkBottomUp()
            .filter { it.isFile }
            .filter { f -> stems.any { stem -> f.name == stem || f.name.startsWith("$stem.") } }
            .forEach { it.delete() }
    }

    private fun canonical(accountIdB64: String): String =
        me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)
}
