// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.account

import android.content.Context
import me.lakko.app.chat.KnownGroupsStore
import me.lakko.app.push.PushRouteStore
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * The stores themselves refuse a write for an account whose session has
 * ended, whoever calls them: after a forget-device nothing of the account is
 * written back, and after a sign-out what is on disk stays as it was.
 */
@RunWith(RobolectricTestRunner::class)
class SealedAccountStoresTest {

    private lateinit var context: Context
    private val account = "DAwMDAwMDAwMDAwMDAwMDA=="
    private val group = "ab".repeat(32)
    private val pushGid = "cd".repeat(32)

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
        AccountStoreGate.resetForTest()
    }

    @After
    fun tearDown() = AccountStoreGate.resetForTest()

    @Test
    fun `nothing is written back after the forget-device wipe`() {
        val known = KnownGroupsStore(context)
        val routes = PushRouteStore(context)
        known.add(account, group)
        routes.put(account, pushGid, group)

        // The wipe, as `AuthRepository.forgetDevice` runs it.
        AccountStoreGate.seal(account) {
            known.clear(account)
            routes.clearForAccount(account)
        }
        // A writer of the forgotten session that gets to its write only now.
        known.add(account, group)
        routes.put(account, pushGid, group)

        assertTrue(known.load(account).isEmpty())
        assertNull(routes.lookup(account, pushGid))
    }

    @Test
    fun `a sign-out deletes nothing and stops later writes`() {
        val known = KnownGroupsStore(context)
        known.add(account, group)

        AccountStoreGate.seal(account)
        known.add(account, "ef".repeat(32))
        known.remove(account, group)

        assertEquals(setOf(group), known.load(account))
    }

    @Test
    fun `the account's next landing writes its stores again`() {
        val known = KnownGroupsStore(context)
        AccountStoreGate.seal(account)
        AccountStoreGate.open(account)
        known.add(account, group)
        assertEquals(setOf(group), known.load(account))
    }

    @Test
    fun `a forgotten account's own profile is not written back`() {
        val raw = me.lakko.app.auth.base64Decode(account)
        val profile = me.lakko.app.profile.OwnProfile(displayName = "Me", username = null, updatedAtMs = 1L)
        me.lakko.app.profile.OwnProfileStore.save(context, raw, profile)

        AccountStoreGate.seal(account) { me.lakko.app.profile.OwnProfileStore.delete(context, raw) }
        me.lakko.app.profile.OwnProfileStore.save(context, raw, profile)

        assertNull(me.lakko.app.profile.OwnProfileStore.load(context, raw))
    }

    @Test
    fun `a refused snapshot write never lets the step it records reach the wire`() {
        // The MLS snapshot store throws for a sealed account (it does not
        // skip): every writer persists BEFORE it publishes, and the throw is
        // what stops the publish.
        try {
            me.lakko.app.chat.IdentitySnapshotStore.putSnapshot(context, account, byteArrayOf(1))
        } catch (_: Throwable) {
            // No Keystore on the host; the open-account write is not the point.
        }
        AccountStoreGate.seal(account)
        try {
            me.lakko.app.chat.IdentitySnapshotStore.putSnapshot(context, account, byteArrayOf(1))
            org.junit.Assert.fail("a refused ratchet persist was reported as made")
        } catch (_: me.lakko.app.StaleBindingException) {
            // expected
        }

        val chat = java.io.File("src/main/kotlin/me/lakko/app/chat/ChatRepository.kt").readText()
        // The best-effort persist passes the refusal on instead of swallowing
        // it (a proposal publishes right after it).
        val bestEffort = chat.substringAfter("    private fun persistSnapshotBestEffort() {").substringBefore("\n    }\n")
        assertTrue(
            bestEffort.indexOf("catch (ended: me.lakko.app.StaleBindingException)") in
                0 until bestEffort.indexOf("catch (_: Throwable)"),
        )
        // The send path: seal the message, persist, and only then hand out
        // what a publish needs.
        val client = java.io.File("src/main/kotlin/me/lakko/app/chat/ChatClient.kt").readText()
        val prepare = client.substringAfter("    public suspend fun prepareSend(").substringBefore("\n    }\n")
        // ADR-0098 — a chat envelope is attested first; the encrypt takes
        // the attested bytes (`sealed`).
        val sealed = prepare.indexOf("group.createApplicationMessage(identity, sealed)")
        val persisted = prepare.indexOf("persistRatchet()")
        val handedOut = prepare.indexOf("return PreparedSend(")
        assertTrue(sealed in 0 until persisted && persisted < handedOut)
        // The commit path (the ADR-0045 sweep): outbox entry, then snapshot,
        // and a failure of either propagates (nothing catches it).
        val outbox = java.io.File("src/main/kotlin/me/lakko/app/chat/CommitOutbox.kt").readText()
        val crashSafe = outbox.substringAfter("internal fun prepareCrashSafeCommit(").substringBefore("\n}\n")
        assertTrue(crashSafe.indexOf("persistEntryOrThrow()") < crashSafe.indexOf("persistSnapshotOrThrow()"))
        assertTrue("catch" !in crashSafe)
    }

    @Test
    fun `founding a group persists the founder's state before any Welcome goes out`() {
        val chat = java.io.File("src/main/kotlin/me/lakko/app/chat/ChatRepository.kt").readText()
        val start = chat.substringAfter("    private suspend fun startGroupWithOnMlsThread(").substringBefore("\n    }\n")
        val persisted = start.indexOf("persistFoundedGroupBeforeWelcome(")
        val published = start.indexOf("publishWelcomeToMailbox(")
        assertTrue(persisted in 0 until published)
        // The persist is the fail-closed one, and the known-groups row is
        // written inside the gate's throwing variant.
        val persist = chat.substringAfter("    private suspend fun persistFoundedGroupBeforeWelcome(").substringBefore("\n    }\n")
        assertTrue("persistSnapshotDurableOrThrow()" in persist)
        assertTrue("AccountStoreGate.writeOrEnd(accountIdB64)" in persist)
        assertTrue("persistSnapshotBestEffort" !in persist)
    }

    @Test
    fun `the known-groups row of a founded group is refused for a sealed account`() {
        AccountStoreGate.seal(account)
        try {
            AccountStoreGate.writeOrEnd(account) { KnownGroupsStore(context).add(account, group) }
            org.junit.Assert.fail("a refused founding persist was reported as made")
        } catch (_: me.lakko.app.StaleBindingException) {
            // expected: the creation stops before its Welcome
        }
        assertTrue(KnownGroupsStore(context).load(account).isEmpty())
    }

    @Test
    fun `the email route's own-action marker survives an earlier sign-out but not a wipe`() {
        val notices = me.lakko.app.auth.AccountNoticeStore.onDevice(context)
        val kind = me.lakko.app.auth.AccountNoticeKind.AccountOpenedByEmail
        // Signed out earlier in this process: the marker written before the
        // landing is kept, so the landing does not show the person their own
        // action as a stranger's.
        AccountStoreGate.seal(account)
        notices.markOwnBeforeLanding(account, kind, 1_000L)
        assertEquals(1_000L, notices.ownActions(account)[kind])
        // The ordinary marker of a landed session is refused while sealed.
        notices.markOwn(account, kind, 2_000L)
        assertEquals(1_000L, notices.ownActions(account)[kind])
        // A wiped account takes neither.
        AccountStoreGate.seal(account) {
            AccountStoreGate.markWiped(account)
            notices.clearAccount(account)
        }
        notices.markOwnBeforeLanding(account, kind, 3_000L)
        assertNull(notices.ownActions(account)[kind])

        val auth = java.io.File("src/main/kotlin/me/lakko/app/auth/AuthRepository.kt").readText()
        val email = auth.substringAfter("    public suspend fun emailRecoverFinish(").substringBefore("\n    }\n")
        val preMark = email.indexOf("noticeStore.markOwnBeforeLanding(it, AccountNoticeKind.AccountOpenedByEmail")
        assertTrue(preMark in 0 until email.indexOf("recover(phrase, newPassword"))
    }

    @Test
    fun `the stores added for key resets and contact keys refuse a sealed account`() {
        AccountStoreGate.seal(account)
        val links = me.lakko.app.contacts.SharedPrefsProvenLinkStore(context)
        links.save(account, emptyList())
        assertTrue(
            context.getSharedPreferences(me.lakko.app.contacts.SharedPrefsProvenLinkStore.PREFS_NAME, Context.MODE_PRIVATE)
                .all.isEmpty(),
        )

        val blobs = HashMap<String, ByteArray>()
        val sent = me.lakko.app.contacts.SealedSentRequestsStore(
            object : me.lakko.app.auth.SealedBlobStorage {
                override fun read(name: String): ByteArray? = blobs[name]
                override fun write(name: String, plaintext: ByteArray) {
                    blobs[name] = plaintext
                }
                override fun delete(name: String) {
                    blobs.remove(name)
                }
            },
        )
        assertEquals(false, sent.save(account, emptyList()))
        AccountStoreGate.open(account)
        assertEquals(true, sent.save(account, emptyList()))
    }

    @Test
    fun `an owed logout's index row is not written for a wiped account`() {
        val store = me.lakko.app.auth.LogoutOwedStore.onDevice(context)
        AccountStoreGate.seal(account) { AccountStoreGate.markWiped(account) }
        store.save(account, me.lakko.app.auth.LogoutOwed(accessToken = "a", accessExpiresAtMs = 1L, refreshToken = "r", refreshExpiresAtMs = 2L))
        assertTrue(store.accounts().isEmpty())
    }
}
