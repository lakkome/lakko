// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import java.util.Base64
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import me.lakko.app.contacts.AcceptedContactsStore
import me.lakko.app.profile.PeerProfile
import me.lakko.app.profile.ProfileDirectory
import me.lakko.app.profile.SharedPrefsProfilePersistence
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * `lakko-backup` v8 — the names this device learned go into the backup and
 * come back through the profile store's ordinary receive gate. Written and
 * read through the real on-device store; the envelope codec is a stand-in
 * (the native core is not loaded on the host).
 */
@RunWith(RobolectricTestRunner::class)
class ReceivedProfilesBackupTest {

    private val account = ByteArray(16) { 0x0A }
    private val accountB64 = Base64.getEncoder().encodeToString(account)
    private val accountHex = account.joinToString("") { "%02x".format(it) }
    private val enc = Base64.getEncoder()
    private fun key(b: Int) = ByteArray(32) { b.toByte() }

    private val contact = key(0x11)
    private val member = key(0x22)
    private val older = key(0x33)
    private val own = key(0x44)

    // Stand-in codec: name | username | stamp | account id.
    private val compose: (PeerProfile) -> ByteArray? = { p ->
        listOf(p.displayName.orEmpty(), p.username.orEmpty(), p.updatedAtMs.toString(), p.accountIdHex.orEmpty())
            .joinToString("\u001f").toByteArray()
    }
    private val decode: (ByteArray) -> PeerProfile? = { b ->
        val f = String(b).split("\u001f")
        if (f.size != 4) null else PeerProfile(f[0].ifEmpty { null }, f[1].ifEmpty { null }, f[2].toULong(), f[3].ifEmpty { null })
    }

    @Before
    fun setUp() = AcceptedContactsStore.bind(account)

    @After
    fun tearDown() = AcceptedContactsStore.unbind()

    private fun directory() = ProfileDirectory(
        persistence = SharedPrefsProfilePersistence(RuntimeEnvironment.getApplication()),
        scope = CoroutineScope(Dispatchers.Unconfined),
        isNewer = { a, b -> a > b },
        passesRule = { true },
    )

    @Test
    fun `names go into the backup contacts first, never the own account, and come back through the gate`() {
        val context = RuntimeEnvironment.getApplication()
        val dir = directory()
        dir.onProfileReceived(enc.encodeToString(member), PeerProfile("Member Mia", null, 30uL, "aa".repeat(16)))
        dir.onProfileReceived(enc.encodeToString(older), PeerProfile("Old Oleg", null, 10uL, "bb".repeat(16)))
        dir.onProfileReceived(enc.encodeToString(contact), PeerProfile("Contact Cleo", null, 5uL, "cc".repeat(16)))
        dir.onProfileReceived(enc.encodeToString(own), PeerProfile("Me", null, 99uL, accountHex))

        val stored = SharedPrefsProfilePersistence(context).loadProfiles(accountB64)
        val out = receivedProfilesForBackup(
            stored = stored,
            contactKeysB64 = setOf(enc.encodeToString(contact)),
            ownKeysB64 = emptySet(),
            ownAccountIdHex = accountHex,
            compose = compose,
        )
        assertEquals(listOf("Contact Cleo", "Member Mia", "Old Oleg"), out.map { decode(it.envelope)!!.displayName })
        assertEquals(enc.encodeToString(contact), enc.encodeToString(out.first().personKey))

        // A fresh device: nothing stored. Restore through the gate.
        context.getSharedPreferences(SharedPrefsProfilePersistence.PREFS, android.content.Context.MODE_PRIVATE).edit().clear().commit()
        val fresh = directory()
        assertFalse(fresh.resolve(enc.encodeToString(member)).primary == "Member Mia")
        val applied = applyReceivedProfilesFromBackup(
            entries = out,
            ownKeysB64 = setOf(enc.encodeToString(own)),
            ownAccountIdHex = accountHex,
            decode = decode,
            offer = { k, p -> fresh.onProfileReceived(k, p) },
        )
        assertEquals(3, applied)
        assertEquals("Member Mia", fresh.resolve(enc.encodeToString(member)).primary)
        assertEquals("Contact Cleo", fresh.resolve(enc.encodeToString(contact)).primary)
    }

    @Test
    fun `a restore never replaces a newer name the device already holds`() {
        val dir = directory()
        dir.onProfileReceived(enc.encodeToString(member), PeerProfile("Newer", null, 50uL, "aa".repeat(16)))
        val entry = uniffi.lakko_ffi_uniffi.BackupReceivedProfile(member, compose(PeerProfile("Older", null, 20uL, "aa".repeat(16)))!!)
        val applied = applyReceivedProfilesFromBackup(listOf(entry), emptySet(), accountHex, decode) { k, p -> dir.onProfileReceived(k, p) }
        assertEquals(0, applied)
        assertEquals("Newer", dir.resolve(enc.encodeToString(member)).primary)
    }
}
