// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import java.util.Base64
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import me.lakko.app.chat.GroupNameStore
import me.lakko.app.chat.HeldCopy
import me.lakko.app.chat.HeldCopyStore
import me.lakko.app.chat.InMemoryHeldCopyBackend
import me.lakko.app.chat.PersistedHeldSend
import me.lakko.app.chat.StoredGroupName
import me.lakko.app.contacts.InMemoryPendingContactsStore
import me.lakko.app.contacts.PendingContact
import me.lakko.app.profile.InMemoryProfilePersistence
import me.lakko.app.profile.PeerProfile
import me.lakko.app.profile.PeerProfileStore
import me.lakko.app.profile.ProfileDirectory
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * State bundle v3: the shared group names (ADR-0051), the profiles
 * (ADR-0048) and the pending contacts with what was written to them
 * (ADR-0079 section 5) reach a newly paired device.
 *
 * The JVM suite has no native library, so the envelope codecs are stand-ins
 * with the same contract: compose from the stored value with its ORIGINAL
 * stamp, decode back, strictly-greater stamp is newer.
 */
@RunWith(RobolectricTestRunner::class)
class StateBundleV3Test {

    private val context get() = RuntimeEnvironment.getApplication()

    /** The sending device's account and the new device's account. */
    private val sender = "AQEBAQEBAQEBAQEBAQEBAQ=="
    private val joiner = "AgICAgICAgICAgICAgICAg=="

    private val groupHex = "ab".repeat(32)
    private val bob = b64(ByteArray(32) { 7 })
    private val own = b64(ByteArray(32) { 9 })

    private val codecs = StateBundle.EnvelopeCodecs(
        composeGroupName = { "GN|${it.updatedAtMs}|${it.name}".toByteArray() },
        decodeGroupName = { bytes ->
            val parts = String(bytes).split("|", limit = 3)
            if (parts.size != 3 || parts[0] != "GN") null else StoredGroupName(parts[2], parts[1].toULong())
        },
        groupNameIsNewer = { a, b -> a > b },
        composeProfile = { "P|${it.updatedAtMs}|${it.accountIdHex.orEmpty()}|${it.displayName.orEmpty()}".toByteArray() },
        decodeProfile = { bytes ->
            val parts = String(bytes).split("|", limit = 4)
            if (parts.size != 4 || parts[0] != "P") {
                null
            } else {
                PeerProfile(parts[3].ifEmpty { null }, null, parts[1].toULong(), parts[2].ifEmpty { null })
            }
        },
    )

    private fun profileStore(account: String = joiner): PeerProfileStore =
        PeerProfileStore(InMemoryProfilePersistence(account)) { a, b -> a > b }

    private fun pending(accountIdHex: String = "11".repeat(16), key: String = b64(ByteArray(32) { 5 })) = PendingContact(
        accountIdHex = accountIdHex,
        signaturePubkeyB64 = key,
        x25519PubkeyB64 = b64(ByteArray(32) { 6 }),
        displayName = "VG2",
        username = "vg2",
        addedAtMs = 1_000L,
        linkIdHex = "22".repeat(16),
    )

    @Test
    fun `a group name, a profile and a pending contact round-trip into the new device`() {
        GroupNameStore.save(context, sender, groupHex, StoredGroupName("Test VG's", 500uL))
        me.lakko.app.profile.SharedPrefsProfilePersistence(context).saveProfiles(
            sender,
            mapOf(
                bob to PeerProfile("Bob", null, 42uL, "33".repeat(16)),
                own to PeerProfile("Me", null, 43uL, null),
            ),
        )
        val contact = pending()
        val held = listOf(
            PersistedHeldSend(contact.queueKey, contact.queueKey, 2_000L, text = "hello"),
            PersistedHeldSend(contact.queueKey, contact.queueKey, 3_000L, text = "are you there"),
        )
        val built = StateBundle.build(
            context = context,
            accountIdB64 = sender,
            knownGroupIdsHex = listOf(groupHex),
            ownPredecessors = emptyList(),
            links = emptyList(),
            ownKeysB64 = setOf(own),
            pendingContacts = listOf(contact),
            heldSends = held,
            codecs = codecs,
        )
        // The own key never travels.
        assertEquals(listOf(bob), built.profiles.map { it.personKeyB64 })
        val decoded = StateBundle.decode(StateBundle.encode(built))
        assertEquals(built, decoded)

        val profiles = profileStore()
        val pendingStore = InMemoryPendingContactsStore()
        val copies = HeldCopyStore(InMemoryHeldCopyBackend()) { 4_000L }
        val applied = StateBundle.apply(
            context, joiner, decoded!!, codecs,
            ownKeysOverride = setOf(own),
            profileOffer = { key, p -> profiles.offer(key, p) },
            pendingStore = pendingStore,
            copyStore = copies,
        )
        assertEquals(1, applied.groupNames)
        assertEquals(1, applied.profiles)
        assertEquals(1, applied.pendingContacts)
        assertEquals(StoredGroupName("Test VG's", 500uL), GroupNameStore.load(context, joiner, groupHex))
        assertEquals("Bob", profiles.get(bob)?.displayName)
        assertEquals(listOf(contact), pendingStore.load(joiner))
        assertEquals(
            listOf("hello", "are you there"),
            copies.copies(joiner, contact.queueKey).map { it.text },
        )
    }

    @Test
    fun `a newer stored name or profile is not overwritten by an older carried one`() {
        GroupNameStore.save(context, joiner, groupHex, StoredGroupName("Newer", 900uL))
        val profiles = profileStore()
        profiles.offer(bob, PeerProfile("Bob (new)", null, 100uL))
        val bundle = StateBundle.Bundle(
            contacts = emptyList(),
            pins = emptyList(),
            knownGroupIdsB64 = emptyList(),
            groupNames = listOf(
                StateBundle.CarriedGroupName(b64(hex(groupHex)), b64("GN|500|Older".toByteArray())),
            ),
            profiles = listOf(StateBundle.CarriedProfile(bob, b64("P|50||Bob (old)".toByteArray()))),
        )
        val applied = StateBundle.apply(
            context, joiner, bundle, codecs,
            ownKeysOverride = emptySet(),
            profileOffer = { key, p -> profiles.offer(key, p) },
            pendingStore = InMemoryPendingContactsStore(),
            copyStore = HeldCopyStore(InMemoryHeldCopyBackend()),
        )
        assertEquals(0, applied.groupNames)
        assertEquals(0, applied.profiles)
        assertEquals("Newer", GroupNameStore.name(context, joiner, groupHex))
        assertEquals("Bob (new)", profiles.get(bob)?.displayName)
    }

    @Test
    fun `a profile under the own key or naming the own account is never stored`() {
        val ownIdHex = "02".repeat(16)
        val profiles = profileStore()
        val stored = StateBundle.applyProfiles(
            entries = listOf(
                StateBundle.CarriedProfile(own, b64("P|1||Me".toByteArray())),
                StateBundle.CarriedProfile(bob, b64("P|1|$ownIdHex|Me again".toByteArray())),
                StateBundle.CarriedProfile(b64(ByteArray(32) { 3 }), b64("garbage".toByteArray())),
            ),
            ownKeysB64 = setOf(own),
            ownAccountIdHex = ownIdHex,
            decode = codecs.decodeProfile,
            offer = { key, p -> profiles.offer(key, p) },
        )
        assertEquals(0, stored)
        assertTrue(profiles.all().isEmpty())
    }

    @Test
    fun `the profile path files a bundle profile for the signed-in account`() {
        val directory = ProfileDirectory(
            persistence = InMemoryProfilePersistence(joiner),
            scope = CoroutineScope(Dispatchers.Unconfined),
            isNewer = { a, b -> a > b },
        )
        assertEquals(
            me.lakko.app.profile.ProfileOffer.STORED,
            directory.offerForAccount(joiner, bob, PeerProfile("Bob", null, 1uL)),
        )
        assertEquals("Bob", directory.resolve(bob).primary)
    }

    @Test
    fun `a carried pending contact is added only when neither accepted nor pending here`() {
        val a = pending("11".repeat(16), b64(ByteArray(32) { 1 }))
        val sameKey = pending("12".repeat(16), b64(ByteArray(32) { 1 }))
        val sameId = pending("11".repeat(16), b64(ByteArray(32) { 2 }))
        val fresh = pending("13".repeat(16), b64(ByteArray(32) { 3 }))
        val acceptedKey = pending("14".repeat(16), b64(ByteArray(32) { 4 }))
        val accepted = me.lakko.app.contacts.AcceptedContact(
            request = me.lakko.app.contacts.IncomingContactRequest(
                messageId = -1,
                fromPubkey = ByteArray(32) { 4 },
                fromAccountId = null,
                fromPseudonym = ByteArray(32),
                validAfterMs = 0uL,
                intro = null,
                createdAt = "",
            ),
            groupIdHex = null,
            displayName = null,
        )
        val out = StateBundle.freshPendingContacts(
            carried = listOf(sameKey, sameId, fresh, acceptedKey),
            accepted = listOf(accepted),
            pending = listOf(a),
        )
        assertEquals(listOf(fresh), out)
    }

    @Test
    fun `each v3 array is bounded and an over-bound or malformed entry refuses the bundle`() {
        val gid = b64(ByteArray(32) { 1 })
        val env = b64(byteArrayOf(1, 2, 3))
        val name = """{"groupIdB64":"$gid","envelopeB64":"$env"}"""
        val prof = """{"personKeyB64":"$bob","envelopeB64":"$env"}"""
        val held = """{"text":"hi","attachments":[],"replyTo":null,"heldAtMs":5}"""
        fun pc(messages: String) =
            """{"accountIdHex":"${"11".repeat(16)}","signaturePubkeyB64":"$bob","x25519PubkeyB64":"$bob",""" +
                """"displayName":"VG2","username":"vg2","addedAtMs":1,"linkIdB64":"${b64(ByteArray(16))}",""" +
                """"heldMessages":[$messages]}"""
        fun wire(names: String = "", profiles: String = "", pending: String = "") =
            """{"v":4,"contacts":[],"pins":[],"knownGroupIdsB64":[],"ownPredecessors":[],""" +
                """"groupNames":[$names],"profiles":[$profiles],"pendingContacts":[$pending],"channelSeeds":[]}"""
        fun n(k: Int, s: String) = List(k) { s }.joinToString(",")
        // Distinct ids and keys (a repeat refuses the bundle on its own).
        fun idOf(i: Int) = b64(java.nio.ByteBuffer.allocate(32).putInt(i).array())
        fun names(k: Int) = List(k) { """{"groupIdB64":"${idOf(it)}","envelopeB64":"$env"}""" }.joinToString(",")
        fun profs(k: Int) = List(k) { """{"personKeyB64":"${idOf(it)}","envelopeB64":"$env"}""" }.joinToString(",")
        fun pcs(k: Int) = List(k) { pc("").replace("11".repeat(16), "%032x".format(it)) }.joinToString(",")

        // Each bound itself is accepted (one array at a time: all of them at
        // once would pass the 1 MiB size cap).
        assertNotNull(StateBundle.decode(wire(names = names(1000)).toByteArray()))
        assertNotNull(StateBundle.decode(wire(profiles = profs(2000)).toByteArray()))
        assertNotNull(StateBundle.decode(wire(pending = pcs(500)).toByteArray()))
        assertNotNull(StateBundle.decode(wire(pending = pc(n(200, held))).toByteArray()))
        assertNull(StateBundle.decode(wire(names = names(1001)).toByteArray()))
        assertNull(StateBundle.decode(wire(profiles = profs(2001)).toByteArray()))
        assertNull(StateBundle.decode(wire(pending = pcs(501)).toByteArray()))
        assertNull(StateBundle.decode(wire(pending = pc(n(201, held))).toByteArray()))
        // Malformed entries.
        assertNull(StateBundle.decode(wire(profiles = """{"personKeyB64":"${b64(ByteArray(31))}","envelopeB64":"$env"}""").toByteArray()))
        assertNull(StateBundle.decode(wire(names = """{"groupIdB64":"$gid"}""").toByteArray()))
        assertNull(StateBundle.decode(wire(names = """{"groupIdB64":"$gid","envelopeB64":"not base64!"}""").toByteArray()))
        assertNull(StateBundle.decode(wire(pending = pc("""{"text":"hi","attachments":[],"heldAtMs":5}""")).toByteArray()))
        assertNull(StateBundle.decode(wire(pending = pc("""{"text":"hi","attachments":[],"replyTo":1.5,"heldAtMs":5}""")).toByteArray()))
        assertNull(
            StateBundle.decode(
                wire(pending = pc("").replace("\"linkIdB64\":\"${b64(ByteArray(16))}\"", "\"linkIdB64\":\"${b64(ByteArray(15))}\"")).toByteArray(),
            ),
        )
        assertNull(StateBundle.decode(wire(pending = pc("").replace("11".repeat(16), "1G".repeat(16))).toByteArray()))
        // A held message carries a reply target and foreign attachment refs.
        val other = StateBundle.decode(
            wire(pending = pc("""{"text":"x","attachments":[{"a":1},{"b":2}],"replyTo":7,"heldAtMs":5}""")).toByteArray(),
        )!!.pendingContacts.single().heldMessages.single()
        assertEquals(7L, other.replyTo)
        assertEquals(2, StateBundle.heldCopyOf(other).attachmentCount)
    }

    @Test
    fun `ADR-0101 - a carried profile is the envelope as received, and one held without it is not carried`() {
        val received = byteArrayOf(4, 5, 6)
        val withEnvelope = PeerProfile("Anna", null, 3uL, envelopeB64 = java.util.Base64.getEncoder().encodeToString(received))
        assertArrayEquals(received, StateBundle.receivedEnvelopeOrNull(withEnvelope))
        assertNull(StateBundle.receivedEnvelopeOrNull(PeerProfile("Found", null, 0uL)))
        val carried = StateBundle.carriedProfiles(
            mapOf(b64(ByteArray(32) { 10 }) to withEnvelope, b64(ByteArray(32) { 11 }) to PeerProfile("Bare", null, 1uL)),
            ownKeysB64 = emptySet(),
            ownAccountIdHex = null,
            compose = StateBundle::receivedEnvelopeOrNull,
        )
        assertEquals(listOf(b64(ByteArray(32) { 10 })), carried.map { it.personKeyB64 })
        assertEquals(java.util.Base64.getEncoder().encodeToString(received), carried.single().envelopeB64)
    }

    @Test
    fun `the builder keeps the first entries in a stable order and skips what does not compose`() {
        val stored = (0 until 3).associate { i -> b64(ByteArray(32) { (10 + i).toByte() }) to PeerProfile("P$i", null, 1uL) }
        val carried = StateBundle.carriedProfiles(
            stored,
            ownKeysB64 = emptySet(),
            ownAccountIdHex = null,
            compose = { p -> if (p.displayName == "P1") null else byteArrayOf(1) },
        )
        assertEquals(stored.keys.sorted().filterIndexed { i, _ -> i != 1 }, carried.map { it.personKeyB64 })
        val names = StateBundle.carriedGroupNames(
            listOf("cc".repeat(32), "aa".repeat(32), "bb".repeat(32)),
            load = { hex -> if (hex.startsWith("bb")) null else StoredGroupName("n", 1uL) },
            compose = { byteArrayOf(1) },
        )
        assertEquals(listOf(b64(hex("aa".repeat(32))), b64(hex("cc".repeat(32)))), names.map { it.groupIdB64 })
    }

    @Test
    fun `a found name is carried without its username, and a username alone not at all`() {
        val named = b64(ByteArray(32) { 10 })
        val bare = b64(ByteArray(32) { 11 })
        val composed = ArrayList<PeerProfile>()
        val carried = StateBundle.carriedProfiles(
            mapOf(named to PeerProfile("Elin Holm", "elin", 0uL), bare to PeerProfile(null, "bert", 0uL)),
            ownKeysB64 = emptySet(),
            ownAccountIdHex = null,
            compose = { p -> composed += p; byteArrayOf(1) },
        )
        assertEquals(listOf(named), carried.map { it.personKeyB64 })
        assertEquals(listOf(PeerProfile("Elin Holm", null, 0uL)), composed)
    }

    @Test
    fun `a pending record maps its link id between hex and base64`() {
        val contact = pending()
        val carried = StateBundle.carriedPendingContacts(listOf(contact), emptyList(), mapOf(
            contact.queueKey to listOf(HeldCopy("copy", 0, null, 9L), HeldCopy("with file", 1, null, 8L)),
        )).single()
        assertEquals(b64(ByteArray(16) { 0x22 }), carried.linkIdB64)
        // A copy whose attachments are in another client's form is not re-carried.
        assertEquals(listOf("copy"), carried.heldMessages.map { it.text })
        assertEquals(contact, StateBundle.pendingRecordOf(carried))
    }

    @Test
    fun `a repeated group id, person key or pending account refuses the bundle`() {
        val gid = b64(ByteArray(32) { 1 })
        val env = b64(byteArrayOf(1, 2, 3))
        fun pc(id: String) =
            """{"accountIdHex":"$id","signaturePubkeyB64":"$bob","x25519PubkeyB64":"$bob",""" +
                """"displayName":"","username":"","addedAtMs":1,"linkIdB64":"${b64(ByteArray(16))}","heldMessages":[]}"""
        fun wire(names: String = "", profiles: String = "", pending: String = "") =
            """{"v":4,"contacts":[],"pins":[],"knownGroupIdsB64":[],"ownPredecessors":[],""" +
                """"groupNames":[$names],"profiles":[$profiles],"pendingContacts":[$pending],"channelSeeds":[]}"""
        val name = """{"groupIdB64":"$gid","envelopeB64":"$env"}"""
        val prof = """{"personKeyB64":"$bob","envelopeB64":"$env"}"""
        assertNotNull(StateBundle.decode(wire(name, prof, pc("11".repeat(16)) + "," + pc("12".repeat(16))).toByteArray()))
        assertNull(StateBundle.decode(wire(names = "$name,$name").toByteArray()))
        assertNull(StateBundle.decode(wire(profiles = "$prof,$prof").toByteArray()))
        assertNull(StateBundle.decode(wire(pending = pc("11".repeat(16)) + "," + pc("11".repeat(16))).toByteArray()))
    }

    @Test
    fun `the builder trims profiles, then group names, then held messages to fit the size cap`() {
        val contact = StateBundle.CarriedContact(bob, "Bob", 1L)
        val pin = StateBundle.CarriedPin(b64(ByteArray(16)), bob, 0L)
        val profiles = (0 until 40).map { i ->
            StateBundle.CarriedProfile(b64(ByteArray(32) { i.toByte() }), b64(ByteArray(300) { 1 }))
        }
        val names = (0 until 40).map { i ->
            StateBundle.CarriedGroupName(b64(ByteArray(32) { i.toByte() }), b64(ByteArray(300) { 2 }))
        }
        val held = (0 until 40).map { i -> StateBundle.CarriedHeldMessage(text = "m".repeat(300), heldAtMs = i.toLong()) }
        val pending = StateBundle.CarriedPendingContact(
            "11".repeat(16), bob, bob, "VG2", "vg2", 1L, b64(ByteArray(16)), held,
        )
        val full = StateBundle.Bundle(
            contacts = listOf(contact),
            pins = listOf(pin),
            knownGroupIdsB64 = listOf(b64(ByteArray(32))),
            groupNames = names,
            profiles = profiles,
            pendingContacts = listOf(pending),
        )
        val fullSize = StateBundle.encode(full).size
        val noProfiles = StateBundle.encode(full.copy(profiles = emptyList())).size
        val noNames = StateBundle.encode(full.copy(profiles = emptyList(), groupNames = emptyList())).size

        // Fits: nothing is trimmed.
        assertEquals(full, StateBundle.fitToSizeCap(full, fullSize))
        // A little over: only profiles go, from the end.
        val a = StateBundle.fitToSizeCap(full, fullSize - 500)
        assertTrue(StateBundle.encode(a).size <= fullSize - 500)
        assertTrue(a.profiles.size in 1 until profiles.size)
        assertEquals(profiles.take(a.profiles.size), a.profiles)
        assertEquals(names, a.groupNames)
        assertEquals(held, a.pendingContacts.single().heldMessages)
        // Below the size with no profiles: group names go next.
        val b = StateBundle.fitToSizeCap(full, noProfiles - 500)
        assertTrue(StateBundle.encode(b).size <= noProfiles - 500)
        assertTrue(b.profiles.isEmpty())
        assertTrue(b.groupNames.size in 1 until names.size)
        assertEquals(names.take(b.groupNames.size), b.groupNames)
        assertEquals(held, b.pendingContacts.single().heldMessages)
        // Below that: held messages go, from the end; the rest stays whole.
        val c = StateBundle.fitToSizeCap(full, noNames - 500)
        assertTrue(StateBundle.encode(c).size <= noNames - 500)
        assertTrue(c.groupNames.isEmpty())
        val kept = c.pendingContacts.single().heldMessages
        assertTrue(kept.size in 1 until held.size)
        assertEquals(held.take(kept.size), kept)
        assertEquals(listOf(contact), c.contacts)
        assertEquals(listOf(pin), c.pins)
        assertEquals(full.knownGroupIdsB64, c.knownGroupIdsB64)
        assertEquals(1, c.pendingContacts.size)
    }

    private fun b64(bytes: ByteArray): String = Base64.getEncoder().encodeToString(bytes)

    private fun hex(s: String): ByteArray = ByteArray(s.length / 2) { s.substring(it * 2, it * 2 + 2).toInt(16).toByte() }
}
