// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import java.util.Base64
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import me.lakko.app.chat.ConversationKind
import me.lakko.app.chat.resolveMemberIdentity
import me.lakko.app.contacts.AcceptedContactsStore
import me.lakko.app.contacts.ProvenLink
import me.lakko.app.contacts.SharedPrefsProvenLinkStore
import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.profile.PeerProfile
import me.lakko.app.profile.ProfileDirectory
import me.lakko.app.profile.ProfileNameSnapshot
import me.lakko.app.profile.SharedPrefsProfilePersistence
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import uniffi.lakko_ffi_uniffi.GroupMember

/**
 * The background notification finds a group member who is not a contact by
 * the name the app stored: profiles are filed the way the running receive
 * paths file them (the chat poll's sender resolution, a contact request's
 * key) into the on-device preferences, and read back the way the background
 * decrypt reads them (a fresh [ProfileNameSnapshot] for the account, the
 * sender's leaf against the roster).
 */
@RunWith(RobolectricTestRunner::class)
class BackgroundSenderNameStoreTest {

    private val account = ByteArray(16) { 0x0A }
    private val accountB64 = Base64.getEncoder().encodeToString(account)
    private val b64 = Base64.getEncoder()

    private fun key(byte: Int) = ByteArray(32) { byte.toByte() }

    // Bob: a device leaf bound to his account key (ADR-0050).
    private val bobLeaf = key(0x11)
    private val bobAccount = key(0x12)

    // Carol: a leaf with no account binding; her person key is the leaf key.
    private val carolLeaf = key(0x21)

    // Dave: bound now, but his profile row was filed under his leaf key.
    private val daveLeaf = key(0x31)
    private val daveAccount = key(0x32)

    // Erin: rotated (a proven step); her profile is under the first key.
    private val erinFirst = key(0x41)
    private val erinNow = key(0x42)
    private val erinLeaf = key(0x43)

    // Frank: a member nothing is known about.
    private val frankLeaf = key(0x51)
    private val frankAccount = key(0x52)

    private val roster = listOf(
        GroupMember(leafIndex = 1u, signaturePublicKey = bobLeaf, accountKey = bobAccount),
        GroupMember(leafIndex = 2u, signaturePublicKey = carolLeaf, accountKey = ByteArray(0)),
        GroupMember(leafIndex = 3u, signaturePublicKey = daveLeaf, accountKey = daveAccount),
        GroupMember(leafIndex = 4u, signaturePublicKey = erinLeaf, accountKey = erinNow),
        GroupMember(leafIndex = 5u, signaturePublicKey = frankLeaf, accountKey = frankAccount),
    )

    @Before
    fun setUp() {
        EnglishForTests.install()
        AcceptedContactsStore.bind(account)
    }

    @After
    fun tearDown() = AcceptedContactsStore.unbind()

    @Test
    fun `a non-contact member is named from the stored profile by every key the running code files under`() {
        val context = RuntimeEnvironment.getApplication()
        val directory = ProfileDirectory(
            persistence = SharedPrefsProfilePersistence(context),
            scope = CoroutineScope(Dispatchers.Unconfined),
            isNewer = { incoming, stored -> incoming > stored },
            passesRule = { true },
        )
        // The chat poll: the sender leaf resolved against the roster at
        // receipt (ChatClient `resolveSender`), the profile filed under it.
        directory.onProfileReceived(resolveMemberIdentity(roster, 1u), PeerProfile("Bob", null, 10uL))
        directory.onProfileReceived(resolveMemberIdentity(roster, 2u), PeerProfile("Carol", null, 10uL))
        // A row under a device's leaf key (filed before the binding).
        directory.onProfileReceived(b64.encodeToString(daveLeaf), PeerProfile("Dave", null, 10uL))
        // A contact request files under the request's account key.
        directory.onProfileReceived(b64.encodeToString(erinFirst), PeerProfile("Erin", null, 10uL))
        SharedPrefsProvenLinkStore(context).save(
            accountB64,
            listOf(ProvenLink("RVJJTg==", b64.encodeToString(erinFirst), b64.encodeToString(erinNow), 1L)),
        )

        // The background decrypt: a fresh snapshot of what is on the device.
        val snapshot = ProfileNameSnapshot.load(context, accountB64)
        assertEquals("Bob", backgroundSenderNameOf(roster, 1u, snapshot::resolve))
        assertEquals("Carol", backgroundSenderNameOf(roster, 2u, snapshot::resolve))
        assertEquals("Dave", backgroundSenderNameOf(roster, 3u, snapshot::resolve))
        assertEquals("Erin", backgroundSenderNameOf(roster, 4u, snapshot::resolve))
        // Nothing stored: no name, and the notification is the generic
        // banner (a defect), never a placeholder word.
        assertNull(backgroundSenderNameOf(roster, 5u, snapshot::resolve))
        assertNull(backgroundSenderNameOf(roster, 9u, snapshot::resolve))
        assertNull(notificationShape(ConversationKind.Group, "Hiking", backgroundSenderNameOf(roster, 5u, snapshot::resolve), "hi"))
        for (leaf in 1u..4u) {
            val shape = notificationShape(ConversationKind.Group, "Hiking", backgroundSenderNameOf(roster, leaf, snapshot::resolve), "hi")
            assertNoPlaceholderWord(shape?.first, shape?.second)
        }
        // And the app resolves each the same way (bubble, member row, title).
        assertEquals("Bob", directory.resolve(bobAccount).primary)
        assertEquals("Carol", directory.resolve(carolLeaf).primary)
    }

    // VG3: a new group member, no contact, nothing stored on this device.
    private val vg3Leaf = key(0x61)
    private val vg3Account = key(0x62)
    private val vg3Roster = roster + GroupMember(leafIndex = 6u, signaturePublicKey = vg3Leaf, accountKey = vg3Account)

    private fun freshDirectory(): ProfileDirectory {
        val context = RuntimeEnvironment.getApplication()
        return ProfileDirectory(
            persistence = SharedPrefsProfilePersistence(context),
            scope = CoroutineScope(Dispatchers.Unconfined),
            isNewer = { incoming, stored -> incoming > stored },
            passesRule = { true },
        )
    }

    @Test
    fun `a profile and a message in one wake name the sender`() {
        val context = RuntimeEnvironment.getApplication()
        val stored = ProfileNameSnapshot.load(context, accountB64)
        // Nothing stored for VG3 (the app has not polled the profile row).
        assertNull(backgroundSenderNameOf(vg3Roster, 6u, stored::resolve))
        // [profile(VG3), chat(VG3)]: the batch applies the profile, then the
        // chat row is named by it.
        val names = withBatchProfile(stored, vg3Roster, 6u, PeerProfile("VG3", null, 5uL)) { a, b -> a > b }
        val sender = backgroundSenderNameOf(vg3Roster, 6u, names::resolve)
        assertEquals("VG3", sender)
        val chat = notificationShape(ConversationKind.Group, "Hiking", sender, "hello")
        assertEquals("Hiking" to "VG3: hello", chat)
        // [profile(VG3), reaction(VG3)]
        val reaction = reactionNotificationBody(
            ProcessedRow(
                kind = ProcessedKind.REACTION,
                text = "",
                attachmentCount = 0,
                senderName = sender,
                senderAccountB64 = b64.encodeToString(vg3Account),
                reactionAdd = true,
                reactionEmoji = "\uD83D\uDC4D",
                reactionTargetAuthorB64 = "OWN=",
            ),
            ownPersonKeyB64 = "OWN=",
            enabled = { true },
            muted = { false },
        )!!
        assertEquals("VG3 reacted \uD83D\uDC4D to your message", reaction)
        val shaped = notificationShape(ConversationKind.Group, "Hiking", sender, reaction, prefixSender = false)
        assertEquals("Hiking" to "VG3 reacted \uD83D\uDC4D to your message", shaped)
        assertNoPlaceholderWord(chat?.first, chat?.second, shaped?.first, shaped?.second)
        // Filed in memory only: what the app stored is unchanged.
        assertNull(backgroundSenderNameOf(vg3Roster, 6u, ProfileNameSnapshot.load(context, accountB64)::resolve))
    }

    @Test
    fun `a batch profile follows the store's rule and a rename still wins`() {
        val context = RuntimeEnvironment.getApplication()
        val directory = freshDirectory()
        directory.onProfileReceived(resolveMemberIdentity(vg3Roster, 6u), PeerProfile("Old name", null, 10uL))
        val stored = ProfileNameSnapshot.load(context, accountB64)
        val newer = withBatchProfile(stored, vg3Roster, 6u, PeerProfile("New name", null, 11uL)) { a, b -> a > b }
        assertEquals("New name", backgroundSenderNameOf(vg3Roster, 6u, newer::resolve))
        val older = withBatchProfile(stored, vg3Roster, 6u, PeerProfile("Stale", null, 9uL)) { a, b -> a > b }
        assertEquals("Old name", backgroundSenderNameOf(vg3Roster, 6u, older::resolve))
    }

    @Test
    fun `in the app a bubble and a membership row are named by the profile of the same batch`() {
        val directory = freshDirectory()
        // The poll resolves both the bubble's author and the profile's sender
        // with the one roster function; the profile is filed first, the
        // bubble and the row draw from the store afterwards.
        val author = resolveMemberIdentity(vg3Roster, 6u)!!
        assertEquals("Member", directory.resolve(author).primary)
        directory.onProfileReceived(resolveMemberIdentity(vg3Roster, 6u), PeerProfile("VG3", null, 5uL))
        assertEquals("VG3", directory.resolve(author).primary)
        // A stored membership row names the person by key, resolved at draw.
        val row = me.lakko.app.chat.ChatMessage(
            messageId = 9L,
            direction = me.lakko.app.chat.Direction.System,
            text = me.lakko.app.chat.COMMIT_ROW_TEXT,
            createdAt = "2026-11-04T10:00:00Z",
            membershipJoined = listOf(author),
        )
        val line = me.lakko.app.chat.membershipRowText(row, { directory.resolve(it).primary }, EnglishForTests.strings)
        assertEquals(EnglishForTests.strings.format("chat.system.member_joined", mapOf("name" to "VG3")), line)
        assertNoPlaceholderWord(line)
    }
}
