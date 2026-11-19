// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0074 section 3.5 x ADR-0073 R.2 - the one filter every path that adds
 * a person's devices goes through ([gateHeldKeyForAdd], [addFilterKeys]).
 *
 * The gate itself and the KeyPackage binding read are native; what is run
 * here is the decision over what the gate leaves in the stores, and a source
 * check that no add path in the chat repository chooses devices any other
 * way.
 */
class AddFilterTest {
    private val old = "key-old"
    private val current = "key-current"
    private val retired = "key-retired"

    /** A directory row: a device and the account key its binding names. */
    private data class Device(val id: String, val boundTo: String?)

    private fun added(devices: List<Device>, accepted: Set<String>): List<String> =
        devicesBoundTo(devices, accepted) { it.boundTo }.map { it.id }

    @Test
    fun `a rotated non-contact is added`() {
        // Before the add: this device verified the person's OLD key against
        // the directory (a binding, no pin). So the add tells the gate
        // nothing and lets it judge the change from that binding.
        assertNull(gateHeldKeyForAdd(pinned = false, hasBinding = true, expectedKeyB64 = current))
        // The gate re-pinned on the proof and recorded the old key as a
        // proven predecessor. The person's devices are still bound to it.
        val accepted = addFilterKeys(
            expectedKeyB64 = current,
            pinnedChainB64 = listOf(old, current),
            bindingKeyB64 = null,
            retiredKeysB64 = emptySet(),
        )
        assertEquals(setOf(current, old), accepted)
        val devices = listOf(Device("kept-phone", old), Device("paired-later", current), Device("someone-else", "other"))
        assertEquals(listOf("kept-phone", "paired-later"), added(devices, accepted))
    }

    @Test
    fun `the roster key of a rotated person accepts the devices bound to the new key too`() {
        val accepted = addFilterKeys(old, listOf(old, current), null, emptySet())
        assertEquals(setOf(old, current), accepted)
    }

    @Test
    fun `a rotation this device has no proof of adds nobody bound to the other key`() {
        // First sight of an account that already rotated: nothing is pinned
        // and no binding was ever recorded, so the gate is told the expected
        // key as a roster key and establishes nothing for the old one.
        assertEquals(current, gateHeldKeyForAdd(pinned = false, hasBinding = false, expectedKeyB64 = current))
        val accepted = addFilterKeys(current, pinnedChainB64 = null, bindingKeyB64 = null, retiredKeysB64 = emptySet())
        assertEquals(setOf(current), accepted)
        assertEquals(emptyList<String>(), added(listOf(Device("kept-phone", old)), accepted))
    }

    @Test
    fun `a reset-retired key is refused`() {
        val retiredKeys = setOf(retired)
        // Named by the caller: nobody is added under it.
        assertEquals(emptySet<String>(), addFilterKeys(retired, listOf(current), null, retiredKeys))
        assertEquals(emptySet<String>(), addFilterKeys(retired, null, null, retiredKeys))
        // Not named, and a device of the account is still bound to it.
        val accepted = addFilterKeys(current, listOf(current), null, retiredKeys)
        assertEquals(setOf(current), accepted)
        assertEquals(
            listOf("new-phone"),
            added(listOf(Device("stolen-phone", retired), Device("new-phone", current)), accepted),
        )
        // A retired key can never ride in through a chain either.
        assertEquals(setOf(current), addFilterKeys(current, listOf(retired, current), null, retiredKeys))
    }

    @Test
    fun `a re-request names no person key and reads the key this device holds`() {
        assertEquals(setOf(current, old), addFilterKeys(null, listOf(old, current), null, emptySet()))
        assertEquals(setOf(old), addFilterKeys(null, null, old, emptySet()))
        assertEquals(emptySet<String>(), addFilterKeys(null, null, null, emptySet()))
        assertEquals(emptySet<String>(), addFilterKeys(null, null, retired, setOf(retired)))
    }

    @Test
    fun `a key outside the pinned chain is accepted alone`() {
        assertEquals(setOf("third"), addFilterKeys("third", listOf(old, current), null, emptySet()))
    }

    @Test
    fun `a device with no readable binding is never added`() {
        assertEquals(emptyList<String>(), added(listOf(Device("unparsed", null)), setOf(current)))
        assertEquals(emptyList<String>(), added(listOf(Device("any", current)), emptySet()))
    }

    @Test
    fun `the gate is never told to trust a key on first use from an add`() {
        // Pinned or bound: the gate judges from what this device holds.
        assertNull(gateHeldKeyForAdd(pinned = true, hasBinding = false, expectedKeyB64 = current))
        assertNull(gateHeldKeyForAdd(pinned = false, hasBinding = true, expectedKeyB64 = current))
        // Neither: the expected key goes in as a ROSTER key, which records a
        // binding when the directory serves exactly it and pins nothing.
        assertEquals(current, gateHeldKeyForAdd(pinned = false, hasBinding = false, expectedKeyB64 = current))
        assertNull(gateHeldKeyForAdd(pinned = false, hasBinding = false, expectedKeyB64 = null))
        assertNull(gateHeldKeyForAdd(pinned = false, hasBinding = false, expectedKeyB64 = ""))
        // A re-request for an account this device trusts no key for names no
        // key either: the gate is not run, since told nothing it would pin
        // whatever the directory serves.
        assertFalse(addRunsGate(pinned = false, hasBinding = false, expectedKeyB64 = null))
        assertFalse(addRunsGate(pinned = false, hasBinding = false, expectedKeyB64 = ""))
        assertTrue(addRunsGate(pinned = false, hasBinding = false, expectedKeyB64 = current))
        assertTrue(addRunsGate(pinned = true, hasBinding = false, expectedKeyB64 = null))
        assertTrue(addRunsGate(pinned = false, hasBinding = true, expectedKeyB64 = null))
    }

    @Test
    fun `every add path goes through the same filter`() {
        val source = File("src/main/kotlin/me/lakko/app/chat/ChatRepository.kt")
        assertTrue("run from the app module: ${source.absolutePath}", source.isFile)
        val lines = source.readLines()

        // The accepted keys are computed in ONE function of the repository.
        val direct = lines.withIndex().filter { (_, l) ->
            l.contains("acceptableBoundKeys(") || l.contains("contacts.addFilterKeys(")
        }
        assertEquals(
            "only addFilterKeysNow may compute the accepted keys: ${direct.map { it.index + 1 }}",
            1,
            direct.size,
        )
        assertEquals(1, lines.count { it.contains("private fun addFilterKeysNow(") })

        // Every read of the directory's device list is followed, within a
        // few lines, by a filter over each KeyPackage's binding: the person
        // filter for another account, the conversation-key rule for this
        // account's own devices. The one exception is the list of this
        // account's other devices put into a group this device founds.
        val filters = listOf("keyPackageBoundTo(", "ownKeyPackageUsable(")
        val unfiltered = lines.withIndex()
            .filter { (_, l) -> l.contains("discovery.fetchKeyPackages(") }
            .filterNot { (i, _) -> lines.subList(i, minOf(lines.size, i + 16)).any { l -> filters.any { l.contains(it) } } }
            .map { (i, _) -> enclosingFunction(lines, i) }
        assertEquals(listOf("ownOtherDevices", "directoryDeviceKeysOf"), unfiltered)

        // And the paths that add ANOTHER person all take their devices from
        // the one function that runs the gate first.
        for (caller in listOf("addMemberToGroupOnMlsThread", "prepareGroupOnMlsThread", "reRequestIntent")) {
            assertTrue(
                "$caller must take the person's devices from personDevicesToAdd",
                bodyOf(lines, caller).any { it.contains("personDevicesToAdd(") },
            )
        }
        val gateFirst = bodyOf(lines, "personDevicesToAdd")
        val gateAt = gateFirst.indexOfFirst { it.contains("contactIdentityCheck?.invoke(") }
        val fetchAt = gateFirst.indexOfFirst { it.contains("discovery.fetchKeyPackages(") }
        assertTrue("the identity gate runs before the directory is read", gateAt in 0 until fetchAt)
    }

    private val functionStart = Regex("""^\s*(?:(?:private|internal|public|override|suspend|inline)\s+)*fun\s+(?:<[^>]+>\s+)?([A-Za-z0-9_]+)\(""")

    private fun enclosingFunction(lines: List<String>, index: Int): String? =
        (index downTo 0).firstNotNullOfOrNull { functionStart.find(lines[it])?.groupValues?.get(1) }

    private fun bodyOf(lines: List<String>, name: String): List<String> {
        val start = lines.indexOfFirst { functionStart.find(it)?.groupValues?.get(1) == name }
        check(start >= 0) { "no function $name" }
        val end = (start + 1 until lines.size).firstOrNull { functionStart.containsMatchIn(lines[it]) } ?: lines.size
        return lines.subList(start, end)
    }
}
