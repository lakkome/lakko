// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.search

import uniffi.lakko_ffi_uniffi.FfiIntroductionPayload
import uniffi.lakko_ffi_uniffi.FfiIntroductionsBundle
import uniffi.lakko_ffi_uniffi.FfiReachGrant

/*
 * Contacts of contacts: opening the introduction records this device keeps
 * (ADR-0060 section 4). The Android twin of `apps/web/src/search/introductions.ts`.
 *
 * A contact relays the records of THEIR contacts as a bundle; a record is
 * sealed under the OPRF output of each search term of the introduced
 * person's name and username, so only someone who searched for one of those
 * terms can open it. The searcher compares the record's tags with
 * `fofTag(oprf)` for the OPRF outputs it holds (the query term's, the
 * username's) and opens only a record whose tag matches.
 *
 * A record that matched a tag and then failed to authenticate is a forgery,
 * not a miss: it is skipped, never shown. What a record claims is the
 * introduced person's claim - the caller still filters by the whole query,
 * checks the identity binding, verifies the username, and runs the reach
 * check before showing it.
 *
 * The native functions arrive as [IntroductionCore], so the rules run on the
 * host JVM.
 */

/** The core functions this module uses. */
public interface IntroductionCore {
    public fun introductionTags(record: ByteArray): List<ByteArray>
    public fun fofTag(oprfOutput: ByteArray): ByteArray
    public fun openIntroduction(record: ByteArray, oprfOutput: ByteArray): FfiIntroductionPayload?
    public fun decodeIntroductionsBundle(plaintext: ByteArray): FfiIntroductionsBundle
    public fun encodeReachGrant(grant: FfiReachGrant): ByteArray
}

/** The production [IntroductionCore]: the UniFFI functions. */
public object FfiIntroductionCore : IntroductionCore {
    override fun introductionTags(record: ByteArray): List<ByteArray> =
        uniffi.lakko_ffi_uniffi.introductionTags(record)

    override fun fofTag(oprfOutput: ByteArray): ByteArray = uniffi.lakko_ffi_uniffi.fofTag(oprfOutput)

    override fun openIntroduction(record: ByteArray, oprfOutput: ByteArray): FfiIntroductionPayload? =
        uniffi.lakko_ffi_uniffi.openIntroduction(record, oprfOutput)

    override fun decodeIntroductionsBundle(plaintext: ByteArray): FfiIntroductionsBundle =
        uniffi.lakko_ffi_uniffi.decodeIntroductionsBundle(plaintext)

    override fun encodeReachGrant(grant: FfiReachGrant): ByteArray =
        uniffi.lakko_ffi_uniffi.encodeReachGrant(grant)
}

/** Which OPRF output a record was opened with. */
public enum class ProbeVia { TERM, USERNAME }

/** One OPRF OUTPUT (never the lookup key) of the username or of the term. */
public class IntroductionProbe(public val oprfOutput: ByteArray, public val via: ProbeVia)

/** What an opened record revealed. */
public class OpenedIntroduction(
    public val identityPk: ByteArray,
    public val accountId: ByteArray,
    public val displayName: String?,
    public val username: String?,
    public val usernameCertificate: ByteArray?,
    /** The contacts-of-contacts grant wire. Secret. */
    public val fofWire: ByteArray,
    public val via: ProbeVia,
)

/**
 * Every record wire the kept introductions hold: each relayed bundle's
 * records (padding included - a dummy never matches) and each contact's own
 * offer. A bundle that fails to decode contributes nothing.
 */
public fun recordsOf(
    bundleWires: Collection<ByteArray>,
    offeredRecords: Collection<ByteArray>,
    core: IntroductionCore,
): List<ByteArray> {
    val out = mutableListOf<ByteArray>()
    for (wire in bundleWires) {
        val bundle = try {
            core.decodeIntroductionsBundle(wire)
        } catch (_: Throwable) {
            continue
        }
        out += bundle.records
    }
    out += offeredRecords
    return out
}

private fun tagsContain(tags: List<ByteArray>, tag: ByteArray): Boolean =
    tags.any { it.size == tag.size && it.contentEquals(tag) }

/**
 * Open every record one of the probes' tags matches. A record matched by two
 * probes is opened once (the first probe wins). A record that opens without
 * a `fof` grant (a public record) is never an introduction. Never throws.
 */
public fun openMatchingIntroductions(
    records: List<ByteArray>,
    probes: List<IntroductionProbe>,
    core: IntroductionCore,
): List<OpenedIntroduction> {
    val probeTags = probes.mapNotNull { probe ->
        try {
            core.fofTag(probe.oprfOutput) to probe
        } catch (_: Throwable) {
            null
        }
    }
    val out = mutableListOf<OpenedIntroduction>()
    for (record in records) {
        val tags = try {
            core.introductionTags(record)
        } catch (_: Throwable) {
            continue
        }
        val hit = probeTags.firstOrNull { tagsContain(tags, it.first) }?.second ?: continue
        val payload = try {
            core.openIntroduction(record, hit.oprfOutput)
        } catch (_: Throwable) {
            // A tag matched and the record did not authenticate: a forgery.
            null
        } ?: continue
        val grant = payload.fof ?: continue
        val fofWire = try {
            core.encodeReachGrant(grant)
        } catch (_: Throwable) {
            continue
        }
        out += OpenedIntroduction(
            identityPk = payload.identityPk,
            accountId = payload.accountId,
            displayName = payload.displayName,
            username = payload.username,
            usernameCertificate = payload.usernameCertificate,
            fofWire = fofWire,
            via = hit.via,
        )
    }
    return out
}
