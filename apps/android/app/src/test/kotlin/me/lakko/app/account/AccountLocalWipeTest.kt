// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.account

import android.content.Context
import java.io.File
import java.util.Base64
import kotlinx.coroutines.runBlocking
import me.lakko.app.anon.AnonTokenContext
import me.lakko.app.anon.FileAnonTokenStore
import me.lakko.app.anon.PersistedAnonPool
import me.lakko.app.anon.PooledAnonToken
import me.lakko.app.auth.PersistedSession
import me.lakko.app.auth.SessionStore
import me.lakko.app.calls.GroupCallRecordRow
import me.lakko.app.calls.SharedPreferencesGroupCallStore
import me.lakko.app.channels.ChannelPublisherKeys
import me.lakko.app.channels.ChannelsUiState
import me.lakko.app.channels.CoPublishedJournals
import me.lakko.app.chat.CommitOutboxStore
import me.lakko.app.chat.ConversationKind
import me.lakko.app.chat.ConversationKindStore
import me.lakko.app.chat.DeviceReadmission
import me.lakko.app.chat.GroupNameStore
import me.lakko.app.chat.KnownGroupsStore
import me.lakko.app.chat.PendingCommitEntry
import me.lakko.app.chat.PinStore
import me.lakko.app.chat.RequestObservation
import me.lakko.app.chat.SharedPreferencesReadWatermarkStore
import me.lakko.app.chat.SharedPrefsBeaconObservationStore
import me.lakko.app.chat.SharedPrefsLastObservedOwnerStore
import me.lakko.app.chat.SharedPrefsLeaveRecordStore
import me.lakko.app.chat.SharedPrefsPendingRequestStore
import me.lakko.app.chat.SharedPrefsUnappliedRequestStore
import me.lakko.app.chat.StoredGroupName
import me.lakko.app.chat.StoredPin
import me.lakko.app.chat.UnappliedKind
import me.lakko.app.chat.UnappliedRow
import me.lakko.app.contacts.AcceptedContact
import me.lakko.app.contacts.AcceptedContactsStore
import me.lakko.app.contacts.IncomingContactRequest
import me.lakko.app.contacts.SharedPreferencesContactsInboxStore
import me.lakko.app.devices.DeviceGroupJournalAccess
import me.lakko.app.devices.StateBundle
import me.lakko.app.devices.deviceLinkAnswerLimit
import me.lakko.app.media.MediaPrefs
import me.lakko.app.profile.OwnProfile
import me.lakko.app.profile.OwnProfileStore
import me.lakko.app.profile.PeerProfile
import me.lakko.app.profile.ProfileRequestPrefsStore
import me.lakko.app.profile.SharedPrefsProfilePersistence
import me.lakko.app.profile.UsernameVerification
import me.lakko.app.push.PushRouteStore
import me.lakko.app.push.PushWakeBindingStore
import me.lakko.app.ui.chat.FolderTabPrefs
import me.lakko.app.ui.chat.GroupPresentationStore
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * Forget device and Delete account leave nothing of the account behind
 * (maintainer rule, 2026-09-30), tested by inventory rather than by listing:
 * every account-scoped store this JVM can reach is seeded for two accounts,
 * [AccountLocalWipe] runs for one, and then the whole of shared preferences,
 * `noBackupFilesDir` and `cacheDir` is walked. Nothing may name the wiped
 * account (in any spelling a store uses: standard base64, the URL-safe
 * unpadded form, the filesystem-safe form, hex), and the other account's
 * rows must still be there.
 *
 * Not reachable here: the stores behind `EncryptedSharedPreferences` (session
 * token, identity seeds, export key, recovery phrase, backup floor, push
 * routing secrets, journal ownership, reach, identity pins, invite-link
 * secrets, channel log + subscriber keys, call history) need the Android
 * Keystore, which Robolectric does not provide. Their steps are asserted to be
 * the only ones that fail, which also proves a failing store never stops the
 * rest. The one exception is the channel publisher store (signing seeds and
 * publish counters): its step is built over plain preferences here
 * ([AccountLocalWipe.channelPublisherKeysStep]) so the walk sees that the
 * account's counter goes together with its signing seeds.
 */
@RunWith(RobolectricTestRunner::class)
class AccountLocalWipeTest {

    private lateinit var context: Context

    // 0xFB bytes spell `+` and `/` in standard base64, so the safe-name and
    // URL-safe spellings differ from the standard one.
    private val rawA = ByteArray(16) { 0xFB.toByte() }
    private val rawB = ByteArray(16) { 0x0B }
    private val a = Base64.getEncoder().encodeToString(rawA)
    private val b = Base64.getEncoder().encodeToString(rawB)
    private val group = "aa".repeat(32)
    private val pushGid = "cc".repeat(32)

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
        AcceptedContactsStore.unbind()
    }

    @After
    fun tearDown() = AcceptedContactsStore.unbind()

    @Test
    fun `the wipe leaves nothing naming the account and keeps the other account's data`() {
        seed(rawA, a, marker = "account-A")
        seed(rawB, b, marker = "account-B")
        seedDeviceWideAndRetired()

        // The session is over before the wipe runs (MainActivity stops the
        // runtimes first).
        AcceptedContactsStore.unbind()
        val failed = AccountLocalWipe.runSteps(context, a, stepsWithPlainChannelPublisherPrefs())

        // Exactly the Keystore-backed stores fail here, and their failure
        // stopped none of the others.
        assertEquals(KEYSTORE_BACKED_STEPS, failed.toSet())

        assertTrue("prefs rows still naming the wiped account: ${prefsLeftovers()}", prefsLeftovers().isEmpty())
        assertTrue("files still naming the wiped account: ${fileLeftovers()}", fileLeftovers().isEmpty())

        // ---- the other account ----------------------------------------------
        val prefsNames = allPrefsNames()
        val spellingsB = spellings(rawB)
        val filesStillNamingB = mutableSetOf<String>()
        for (name in prefsNames) {
            for ((key, value) in context.getSharedPreferences(name, Context.MODE_PRIVATE).all) {
                if (spellingsB.any { it in "$key=$value" }) filesStillNamingB += name
            }
        }
        val missingB = PREFS_SEEDED_PER_ACCOUNT - filesStillNamingB
        assertTrue("the other account lost its rows in: $missingB", missingB.isEmpty())
        val bFiles = mutableListOf<String>()
        for (root in listOf(context.noBackupFilesDir, context.cacheDir)) {
            root.walkTopDown().filter { it.isFile }.forEach { f ->
                val content = runCatching { f.readText() }.getOrDefault("")
                if (spellingsB.any { it in f.name } || "account-B" in content) bFiles += f.relativeTo(root).path
            }
        }
        assertEquals("the other account's files stay: $bFiles", FILES_SEEDED_PER_ACCOUNT, bFiles.size)

        // ---- the device-wide rows -------------------------------------------
        // Retired rows (never read, possibly holding this account's data) go.
        for ((prefs, key) in RETIRED_ROWS) {
            assertFalse("retired $prefs/$key survived", context.getSharedPreferences(prefs, Context.MODE_PRIVATE).contains(key))
        }
        assertFalse("retired played file survived", File(context.noBackupFilesDir, "lakko-media-played.txt").exists())
        assertFalse("retired token pool survived", File(context.noBackupFilesDir, "lakko-anon-tokens/pool.json").exists())
        // Plaintext caches go.
        for (dir in listOf("chat-media", "chat-media-outbox", "shared-attachments")) {
            assertFalse("cache $dir survived", File(context.cacheDir, dir).exists())
        }
        // Genuine device settings stay.
        assertEquals("dark", context.getSharedPreferences("lakko-appearance", Context.MODE_PRIVATE).getString("theme", null))
        assertTrue(context.getSharedPreferences("lakko.notifications", Context.MODE_PRIVATE).getBoolean("asked", false))
        assertEquals(
            "video_note",
            context.getSharedPreferences("lakko-media-prefs", Context.MODE_PRIVATE).getString("composer.recordMode", null),
        )
        // The other account's recent emoji and tokens stay readable.
        AcceptedContactsStore.bind(rawB)
        assertEquals(listOf("\uD83D\uDE00"), MediaPrefs(context).recentEmoji)
        AcceptedContactsStore.unbind()
        assertEquals(1, FileAnonTokenStore.under(context.noBackupFilesDir).load(b)!!.tokens.size)
        assertEquals(null, FileAnonTokenStore.under(context.noBackupFilesDir).load(a))
        // The other account's channel publisher key and its counter floor
        // stay together; the wiped account's both went.
        val publisherPrefs = context.getSharedPreferences(CHANNEL_PUBLISHER_PREFS, Context.MODE_PRIVATE)
        for (row in listOf(ChannelsUiState.PUBLISHER_COUNTERS_PREF_KEY, ChannelsUiState.PUBLISHER_SIGNING_SEEDS_PREF_KEY)) {
            assertTrue("$row of the other account", publisherPrefs.contains(ChannelsUiState.accountKey(row, b)))
            assertFalse("$row of the wiped account", publisherPrefs.contains(ChannelsUiState.accountKey(row, a)))
        }
        val channelsB = ChannelsUiState(publisherPrefs = publisherPrefs)
        runBlocking { channelsB.bind(b, ByteArray(64)) }
        assertEquals(1uL, channelsB.nextCounter(CHANNEL))
    }

    @Test
    fun `the walk notices a store the wipe left out`() {
        // Negative control: the same seed, the wipe without two steps (one
        // preferences store, one file store). The walk must name both, or
        // the test above would pass on a wipe that missed them.
        seed(rawA, a, marker = "account-A")
        AcceptedContactsStore.unbind()
        val partial = AccountLocalWipe.steps.filterNot { it.name == "known-groups" || it.name == "message-log" }
        AccountLocalWipe.runSteps(context, a, partial)
        assertTrue(prefsLeftovers().any { it.startsWith("me.lakko.known-groups.v1:") })
        // The file backstop sweeps the message log even with its step gone.
        assertTrue(fileLeftovers().isEmpty())
        val noBackstop = partial.filterNot { it.name == "no-backup-files-sweep" }
        seed(rawA, a, marker = "account-A")
        AccountLocalWipe.runSteps(context, a, noBackstop)
        assertTrue(fileLeftovers().any { it.startsWith("lakko-message-logs/") })
    }

    @Test
    fun `the wipe of one account never touches another account's session record`() {
        SessionStore(context).save(PersistedSession(accountIdB64 = b, username = "bob"))
        AccountLocalWipe.wipeAccountLocalData(context, a)
        assertEquals(b, SessionStore(context).load()?.accountIdB64)
    }

    @Test
    fun `file names are swept in every spelling a store uses`() {
        val root = File(context.noBackupFilesDir, "some-future-store").apply { mkdirs() }
        val stems = AccountLocalWipe.fileNameStems(a)
        for (stem in stems) File(root, "$stem.bin").writeText("x")
        File(root, "${b.replace('/', '_')}.bin").writeText("y")
        AccountLocalWipe.sweepFilesNamedBy(context.noBackupFilesDir, a)
        assertEquals(listOf("${b.replace('/', '_')}.bin"), root.list()!!.toList())
    }

    // ---------------------------------------------------------------------

    /** The production steps, with the channel publisher step opening plain
     *  preferences instead of the Keystore-backed store. */
    private fun stepsWithPlainChannelPublisherPrefs(): List<AccountLocalWipe.Step> {
        val plain = AccountLocalWipe.channelPublisherKeysStep { ctx ->
            ctx.getSharedPreferences(CHANNEL_PUBLISHER_PREFS, Context.MODE_PRIVATE)
        }
        check(AccountLocalWipe.steps.count { it.name == plain.name } == 1)
        return AccountLocalWipe.steps.map { if (it.name == plain.name) plain else it }
    }

    private fun seed(raw: ByteArray, acct: String, marker: String) {
        // Shared preferences, through each store's own write path.
        me.lakko.app.backup.AutoBackupStore.markDirty(context, acct, 1_000L)
        DeviceGroupJournalAccess.markFoundingUnconfirmed(context, acct)
        StateBundle.saveKnownGroupIds(context, acct, listOf(group))
        check(deviceLinkAnswerLimit(context, acct).tryConsume())
        DeviceReadmission.recordRemoved(context, acct, group, before = listOf("k1", "k2"), after = listOf("k1"))
        KnownGroupsStore(context).add(acct, group)
        SharedPrefsBeaconObservationStore(context).save(acct, group, mapOf("key" to 1L))
        SharedPrefsPendingRequestStore(context).save(acct, group, mapOf("ref" to RequestObservation(atMs = 1L, own = true)))
        SharedPrefsUnappliedRequestStore(context).save(acct, group, mapOf("join:ref" to UnappliedRow(UnappliedKind.JOIN_APPROVE, "ref")))
        SharedPrefsLastObservedOwnerStore(context).save(acct, group, "owner")
        SharedPrefsLeaveRecordStore(context).save(acct, group, 5L)
        GroupNameStore.save(context, acct, group, StoredGroupName("Family", 1uL))
        PinStore.saveShared(context, acct, group, mapOf(1L to StoredPin(pinned = true, updatedAtMs = 1uL)))
        PinStore.savePersonal(context, acct, group, mapOf(2L to 1uL))
        SharedPreferencesReadWatermarkStore(context).merge(acct, group, mapOf("member" to 3L))
        SharedPreferencesGroupCallStore(context).save(
            acct,
            group,
            GroupCallRecordRow("dd".repeat(16), audio = true, video = false, initiatorLeaf = 0, startedAtMs = 1, joinedMembers = emptyMap(), lastActivityMs = 1),
        )
        SharedPrefsProfilePersistence(context).saveProfiles(acct, mapOf("sig" to PeerProfile("Peer", null, 1uL)))
        SharedPrefsProfilePersistence(context).saveVerifications(acct, mapOf("sig|peer" to UsernameVerification.VERIFIED))
        OwnProfileStore.save(context, raw, OwnProfile(displayName = "Me", username = null, updatedAtMs = 1L))
        ProfileRequestPrefsStore(context).markRequested(acct, group, 1uL)
        AcceptedContactsStore.save(context, acct, listOf(AcceptedContact(request = request())))
        SharedPreferencesContactsInboxStore(context).saveCursor(acct, 9L)
        CoPublishedJournals.onDevice(context) { acct }.mark("ab".repeat(32))
        PushRouteStore(context).put(acct, pushGid, group)
        context.getSharedPreferences(PushRouteStore.RETIRED_PREFS_NAME, Context.MODE_PRIVATE).edit()
            .putString(if (acct == a) "01".repeat(32) else "02".repeat(32), "$acct $group").commit()
        context.getSharedPreferences("lakko-device-sync-pending", Context.MODE_PRIVATE).edit()
            .putBoolean(acct, true).commit()
        // The stores that write for the bound account.
        AcceptedContactsStore.bind(raw)
        ConversationKindStore.set(context, group, ConversationKind.Group)
        GroupPresentationStore.markCreatedAsGroup(context, group)
        FolderTabPrefs.save(context, "ee".repeat(8))
        MediaPrefs(context).apply {
            recentEmoji = listOf("\uD83D\uDE00")
            tonePrefs = mapOf("\uD83D\uDC4D" to "\uD83D\uDC4D\uD83C\uDFFD")
        }
        AcceptedContactsStore.unbind()
        // A channel this account publishes to: its signing seed and the
        // publish counter guarding it (one post sent).
        ChannelsUiState(
            publisherPrefs = context.getSharedPreferences(CHANNEL_PUBLISHER_PREFS, Context.MODE_PRIVATE),
        ).apply {
            runBlocking { bind(acct, ByteArray(64)) }
            setKeys(CHANNEL, ChannelPublisherKeys(seed = ByteArray(32) { 3 }, signingKeySeed = ByteArray(32) { 4 }))
            nextCounter(CHANNEL)
        }
        // The account's anonymous-token pool (account data since 2026-10-01).
        FileAnonTokenStore.under(context.noBackupFilesDir).save(
            PersistedAnonPool(acct, listOf(PooledAnonToken(AnonTokenContext.DS_KNOCK, 1, "tok-$marker"))),
        )

        // Files: the Keystore-wrapped envelopes are opaque bytes on disk, so
        // raw files at the stores' paths stand in for them.
        val safe = acct.replace('/', '_')
        val nb = context.noBackupFilesDir
        for (path in listOf(
            "lakko-device-keys/$safe.bin",
            "lakko-mls-snapshots/$safe.bin",
            "lakko-mls-snapshots/$safe.leaf",
            "lakko-mls-snapshots/$safe.channels.bin",
            "lakko-mls-snapshots/$safe.bin.tmp",
            "lakko-message-logs/$safe.bin",
            "lakko-device-group-snapshots/$safe.bin",
            "lakko-device-sync-state/$safe.bin",
            // The played marks (the process-wide MediaPrefs is not used here:
            // a singleton made by another test would hold another app dir).
            "lakko-media-played/${MediaPrefs.playedFileName(acct)}",
        )) {
            File(nb, path).apply { parentFile?.mkdirs() }.writeText(marker)
        }
        CommitOutboxStore.putEntry(
            context,
            PendingCommitEntry(id = "entry-$marker", accountId = acct, groupId = group, targetEpoch = "2", commitB64 = "AA=="),
        )
    }

    private fun seedDeviceWideAndRetired() {
        for ((prefs, key) in RETIRED_ROWS) {
            context.getSharedPreferences(prefs, Context.MODE_PRIVATE).edit().putString(key, "retired").commit()
        }
        File(context.noBackupFilesDir, "lakko-media-played.txt").writeText("voice-0")
        // The retired device-wide token pool of builds before 2026-10-01,
        // last filled by A.
        File(context.noBackupFilesDir, "lakko-anon-tokens/pool.json").apply { parentFile?.mkdirs() }
            .writeText("""{"v":1,"owner":"$a","tokens":[]}""")
        PushWakeBindingStore(context).bind(ByteArray(32) { 0x0A }, a)
        SessionStore(context).save(PersistedSession(accountIdB64 = a, username = "alice"))
        for (path in listOf("chat-media/s1/e1/voice.m4a", "chat-media-outbox/take", "shared-attachments/u1/photo.jpg")) {
            File(context.cacheDir, path).apply { parentFile?.mkdirs() }.writeText("plaintext")
        }
        // Genuine device settings.
        context.getSharedPreferences("lakko-appearance", Context.MODE_PRIVATE).edit().putString("theme", "dark").commit()
        context.getSharedPreferences("lakko.notifications", Context.MODE_PRIVATE).edit().putBoolean("asked", true).commit()
        context.getSharedPreferences("lakko-media-prefs", Context.MODE_PRIVATE).edit()
            .putString("composer.recordMode", "video_note").commit()
    }

    private fun request() = IncomingContactRequest(
        messageId = 1L,
        fromPubkey = ByteArray(32) { 1 },
        fromAccountId = null,
        fromPseudonym = ByteArray(32),
        validAfterMs = 1_700_000_000_000uL,
        intro = null,
        createdAt = "2026-09-30T10:00:00Z",
    )

    /** Every preferences row whose key or value names account A. */
    private fun prefsLeftovers(): List<String> {
        val spellingsA = spellings(rawA)
        val out = mutableListOf<String>()
        for (name in allPrefsNames()) {
            for ((key, value) in context.getSharedPreferences(name, Context.MODE_PRIVATE).all) {
                val text = "$key=$value"
                if (spellingsA.any { it in text }) out += "$name: $text"
            }
        }
        return out
    }

    /** Every file under `noBackupFilesDir` or `cacheDir` whose name or
     *  content names account A. */
    private fun fileLeftovers(): List<String> {
        val spellingsA = spellings(rawA)
        val out = mutableListOf<String>()
        for (root in listOf(context.noBackupFilesDir, context.cacheDir)) {
            root.walkTopDown().filter { it.isFile }.forEach { f ->
                val content = runCatching { f.readText() }.getOrDefault("")
                if (spellingsA.any { it in f.name || it in content } || "account-A" in content) {
                    out += f.relativeTo(root).path
                }
            }
        }
        return out
    }

    private fun spellings(raw: ByteArray): Set<String> {
        val std = Base64.getEncoder().encodeToString(raw)
        return setOf(
            std,
            std.replace('/', '_'),
            Base64.getUrlEncoder().withoutPadding().encodeToString(raw),
            raw.joinToString("") { "%02x".format(it.toInt() and 0xff) },
        )
    }

    /** Every preferences file on disk, plus every plain one this client
     *  names (a file written with `apply()` may not be on disk yet). */
    private fun allPrefsNames(): Set<String> {
        val onDisk = File(context.applicationInfo.dataDir, "shared_prefs").listFiles().orEmpty()
            .map { it.name.removeSuffix(".xml") }
        return onDisk.toSet() + ALL_PLAIN_PREFS + CHANNEL_PUBLISHER_PREFS
    }

    private companion object {
        /** Plain stand-in for the Keystore-backed channel publisher store. */
        const val CHANNEL_PUBLISHER_PREFS = "test-channel-publisher-keys"

        val CHANNEL = "12".repeat(16)

        /** Every plain `SharedPreferences` file the app writes (the inventory
         *  of 2026-09-30), device settings included. */
        val ALL_PLAIN_PREFS = setOf(
            "me.lakko.session.v1", "lakko.backup.auto", "lakko-group-calls", "me.lakko.ds.co-published",
            "lakko-committer-beacons", "lakko-conversation-kind", "lakko-readmission-removed", "lakko-group-names",
            "me.lakko.known-groups.v1", "lakko-leave-requested", "lakko-last-observed-owner", "lakko-pending-requests",
            "lakko-pins", "lakko-read-watermarks", "lakko-unapplied-requests", "lakko-accepted-contacts",
            "lakko-contacts-inbox", "me.lakko.devicegroup.founding", "lakko-device-link", "lakko-pairing-known-groups",
            "lakko-device-sync-pending", "lakko-media-prefs", "lakko-own-profile", "lakko-peer-profiles",
            "lakko-profile-requests", "lakko.notifications", "me.lakko.push-routing.v2", "me.lakko.push-routing.v1",
            "me.lakko.push-wake-binding.v1", "lakko-chat-folders", "lakko-group-presentation", "lakko-appearance",
            "lakko.anticensorship", "lakko.release",
        )

        /** The files [seed] writes a row naming the account into. */
        val PREFS_SEEDED_PER_ACCOUNT = setOf(
            "lakko.backup.auto", "me.lakko.devicegroup.founding", "lakko-pairing-known-groups", "lakko-device-link",
            "lakko-readmission-removed", "me.lakko.known-groups.v1", "lakko-committer-beacons", "lakko-pending-requests",
            "lakko-unapplied-requests", "lakko-last-observed-owner", "lakko-leave-requested", "lakko-group-names",
            "lakko-pins", "lakko-read-watermarks", "lakko-group-calls", "lakko-peer-profiles", "lakko-own-profile",
            "lakko-profile-requests", "lakko-accepted-contacts", "lakko-contacts-inbox", "me.lakko.ds.co-published",
            "me.lakko.push-routing.v2", "me.lakko.push-routing.v1", "lakko-device-sync-pending",
            "lakko-conversation-kind", "lakko-group-presentation", "lakko-chat-folders", "lakko-media-prefs",
            CHANNEL_PUBLISHER_PREFS,
        )

        /** Eight Keystore-envelope files, one played-marks file, one
         *  commit-outbox entry and one anonymous-token pool per account. */
        const val FILES_SEEDED_PER_ACCOUNT = 11

        /** Device-wide rows of builds before 2026-09-30, never read. */
        val RETIRED_ROWS = listOf(
            "lakko-accepted-contacts" to "accepted.v1",
            "lakko-conversation-kind" to "kind:${"aa".repeat(32)}",
            "lakko-pairing-known-groups" to "known-groups.v1",
            "lakko-pairing-known-groups" to "restored.v1",
            "lakko-chat-folders" to "selected-folder",
            "lakko-group-presentation" to "created-as-group",
            "me.lakko.ds.co-published" to "co-published.v1",
            "lakko-peer-profiles" to "profiles.v1",
            "lakko-peer-profiles" to "verifications.v1",
            "lakko-device-link" to "answered-at-v1",
            "lakko-media-prefs" to "emoji.recent",
            "lakko-media-prefs" to "emoji.tones",
        )

        /**
         * Steps that open `EncryptedSharedPreferences` (Android Keystore) and
         * so fail here. Two more stores need the Keystore but absorb the
         * failure inside the store and never throw: `invite-link-secrets`
         * (silently) and `call-history` (it logs). On a device both work; here
         * neither can be observed.
         */
        val KEYSTORE_BACKED_STEPS = setOf(
            "push-routing-secrets", "session-token", "account-identity-seed", "device-identity-seed", "export-key",
            "recovery-phrase", "backup-floor", "journal-ownership", "reach", "identity-pins",
            "channel-log-and-keys",
        )
    }
}
